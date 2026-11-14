//! Persistent CDP WebSocket connection with single-reader event loop.
//!
//! Design: exactly one task owns the `WsRead` half. `send_recv` never reads the
//! socket — it publishes a pending `oneshot::Sender` into a shared map, writes
//! the request through the (mutex-guarded) `WsWrite`, and awaits the response
//! on the receiver. Events that arrive without a matching id are broadcast on a
//! `tokio::sync::broadcast` channel for `wait_for_event` subscribers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
use std::time::Duration;

/// Process-wide registry of live CDP connections. Each `CdpConnection` is
/// per-fetch (opened in `fetch_with_ws`, closed in the same call), so any
/// telemetry sampler needs a way to find currently-live connections without
/// holding strong references that would extend their lifetime.
///
/// The registry stores a `Weak` to the per-connection `pending` map (which is
/// the natural liveness sentinel — it's dropped when both the `CdpConnection`
/// and its event loop are gone) plus a clone of the broadcast `Sender` so we
/// can read `receiver_count()` without touching the connection.
pub(crate) struct LiveConnEntry {
    pub pending: Weak<DashMap<u64, oneshot::Sender<CdpResult>>>,
    pub events: broadcast::Sender<CdpEvent>,
}

pub(crate) static LIVE_CONNS: LazyLock<StdMutex<Vec<LiveConnEntry>>> =
    LazyLock::new(|| StdMutex::new(Vec::new()));

/// Snapshot live connections, GCing dead entries inline.
/// Returns (live_count, pending_total, subscribers_total).
pub fn snapshot_live_conns() -> (usize, usize, usize) {
    let mut g = LIVE_CONNS.lock().unwrap();
    // Drop entries whose `pending` Arc is gone — i.e. CdpConnection + its
    // event loop have both been dropped.
    g.retain(|e| e.pending.strong_count() > 0);
    let mut pending_total = 0usize;
    let mut subs_total = 0usize;
    for e in g.iter() {
        if let Some(p) = e.pending.upgrade() {
            pending_total += p.len();
        }
        subs_total += e.events.receiver_count();
    }
    (g.len(), pending_total, subs_total)
}

use crw_core::error::{CrwError, CrwResult};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Uri, header::HOST};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Classify a tungstenite error into a category name without leaking the
/// underlying URL or HTTP response details. Detail goes to traces; this
/// returns just the category for inclusion in user-facing errors.
fn classify_ws_error(e: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::ConnectionClosed | E::AlreadyClosed => "connection closed",
        E::Io(_) => "io error",
        E::Tls(_) => "tls error",
        E::Url(_) => "invalid websocket url",
        E::Http(_) => "http handshake rejected",
        E::HttpFormat(_) => "http format error",
        E::Capacity(_) => "message too large",
        E::Protocol(_) => "websocket protocol error",
        E::WriteBufferFull(_) => "write buffer full",
        E::Utf8(_) => "invalid utf-8",
        E::AttackAttempt => "rejected websocket attack",
    }
}

/// Build the CDP handshake request, forcing a `Host` header Chromium accepts.
///
/// Chromium 148+ guards the DevTools WebSocket against DNS rebinding by
/// validating the `Host` header: only `localhost` and IP literals pass, so a
/// connect over a docker service name (`ws://chrome:9222/...`) is rejected with
/// an HTTP handshake error. tungstenite seeds `Host` from the URL authority, so
/// a self-host compose pointing the engine at the `chrome` service failed.
///
/// Overriding the header instead of rewriting the URL host keeps the hostname in
/// the URI, and the URI host is what `TcpStream::connect` resolves. It then
/// walks the resolver's addresses serially, within the connect timeout, rather
/// than dying on the single one we would have pinned. That is what a
/// `ws://localhost` endpoint needs when `localhost` resolves to `::1` first and
/// the browser listens on `127.0.0.1` only: the refusal is immediate and the
/// walk reaches the v4 address. An address that blackholes SYNs instead will
/// still eat the whole budget, as it did before.
///
/// Narrow on purpose:
/// - `wss://` is untouched. Chromium's DevTools endpoint is plaintext, so the
///   guard is not in play, and a hosted CDP endpoint needs its real Host, SNI
///   and certificate name.
/// - An IP-literal host is untouched; the header tungstenite generates for it
///   already passes the guard.
pub(crate) fn cdp_ws_request(ws_url: &str) -> CrwResult<WsRequest> {
    // Two tolerances `http::Uri` does not have but the `url::Url` parse this
    // replaced did. Both are reachable: `cdp.rs` hands `/devtools/` and `token=`
    // URLs straight here without going through discovery.
    //
    // Surrounding whitespace: `http::Uri` rejects it with InvalidUriChar, and a
    // compose `.env` or TOML value can carry it (the renderer already filters on
    // `ws_url.trim()` in `lib.rs`). Trim rather than fail every render on a space.
    let ws_url = ws_url.trim();
    // Scheme case: `http::Uri` keeps it and tungstenite matches only lowercase
    // `ws` / `wss`. Lowercase the scheme and nothing else, because rewriting any
    // more of the URL is exactly what this function exists to stop doing.
    let lowered = ws_url.split_once("://").and_then(|(scheme, rest)| {
        scheme
            .bytes()
            .any(|b| b.is_ascii_uppercase())
            .then(|| format!("{}://{rest}", scheme.to_ascii_lowercase()))
    });
    let ws_url = lowered.as_deref().unwrap_or(ws_url);
    let mut req = ws_url.into_client_request().map_err(|e| {
        // Same sanitization as the connect error below: the URL may be
        // config- or header-influenceable, so only a category reaches the caller.
        tracing::warn!(error = %e, "CDP request build failed");
        CrwError::RendererError(format!("CDP connect failed: {}", classify_ws_error(&e)))
    })?;
    if let Some(host) = localhost_host_header(req.uri()) {
        req.headers_mut().insert(HOST, host);
    }
    Ok(req)
}

/// `Some(value)` when the `Host` header must be forced to `localhost` for
/// Chromium's rebinding guard, `None` when the generated header already passes.
fn localhost_host_header(uri: &Uri) -> Option<HeaderValue> {
    // Plaintext only. Chromium's DevTools endpoint is never TLS, and a hosted
    // `wss://` CDP endpoint needs its real Host for routing, SNI and cert name.
    if uri.scheme_str() != Some("ws") {
        return None;
    }
    let host = uri.host()?;
    // `Uri::host` keeps the brackets on an IPv6 literal; `IpAddr` rejects them.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let value = match uri.port_u16() {
        Some(port) => format!("localhost:{port}"),
        None => "localhost".to_string(),
    };
    HeaderValue::from_str(&value).ok()
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = futures::stream::SplitSink<WsStream, Message>;
type WsRead = futures::stream::SplitStream<WsStream>;

/// Event or response payload from the remote CDP peer.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: serde_json::Value,
    pub session_id: Option<String>,
}

/// Result type delivered through a pending oneshot: Ok(result) or Err(message).
pub type CdpResult = Result<serde_json::Value, String>;

pub struct CdpConnection {
    write: Arc<Mutex<WsWrite>>,
    pending: Arc<DashMap<u64, oneshot::Sender<CdpResult>>>,
    events: broadcast::Sender<CdpEvent>,
    next_id: Arc<AtomicU64>,
    is_closed: Arc<AtomicBool>,
    event_loop: Option<JoinHandle<()>>,
}

impl CdpConnection {
    /// Open a WebSocket to the given CDP endpoint and spawn the reader loop.
    pub async fn connect(ws_url: &str, connect_timeout: Duration) -> CrwResult<Self> {
        let request = cdp_ws_request(ws_url)?;
        let (ws, _) = tokio::time::timeout(connect_timeout, connect_async(request))
            .await
            .map_err(|_| CrwError::Timeout(connect_timeout.as_millis() as u64))?
            .map_err(|e| {
                // tungstenite's Display can echo the full ws_url back (Url
                // variant) or HTTP response details. The ws_url may be
                // attacker-influenceable via config / proxy headers, so we
                // log the raw error for operators and surface only a
                // sanitized class name to the caller. This keeps prod
                // error responses free of WebSocket URLs and embedded paths.
                tracing::warn!(error = %e, "CDP connect failed");
                CrwError::RendererError(format!("CDP connect failed: {}", classify_ws_error(&e)))
            })?;
        let (write, read) = ws.split();

        let write = Arc::new(Mutex::new(write));
        let pending: Arc<DashMap<u64, oneshot::Sender<CdpResult>>> = Arc::new(DashMap::new());
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let is_closed = Arc::new(AtomicBool::new(false));

        let event_loop = tokio::spawn(run_event_loop(
            read,
            pending.clone(),
            events_tx.clone(),
            is_closed.clone(),
        ));

        // Register with the process-wide live-connection registry. The Weak
        // is held until the connection's pending Arc has refcount 0 (i.e.
        // both this struct and its event loop are gone), at which point
        // snapshot_live_conns() GCs the entry.
        LIVE_CONNS.lock().unwrap().push(LiveConnEntry {
            pending: Arc::downgrade(&pending),
            events: events_tx.clone(),
        });

        Ok(Self {
            write,
            pending,
            events: events_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            is_closed,
            event_loop: Some(event_loop),
        })
    }

    /// Send a CDP command and await its response. Events are filtered out by
    /// the event loop — this call only completes on a message with matching id.
    pub async fn send_recv(
        &self,
        method: &str,
        params: serde_json::Value,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> CrwResult<serde_json::Value> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CrwError::RendererError("CDP connection closed".into()));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut req = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            req["sessionId"] = serde_json::Value::String(sid.to_string());
        }

        let (tx, rx) = oneshot::channel::<CdpResult>();
        self.pending.insert(id, tx);
        // RAII cleanup: if the caller's future is dropped (cancel) between here
        // and the `rx` await below, the guard removes the pending entry so it
        // doesn't leak. On normal response delivery, `dispatch` already
        // removed the entry — `pending.remove` is then a cheap no-op.
        let _cleanup = PendingCleanup {
            pending: &self.pending,
            id,
        };

        {
            let mut write = self.write.lock().await;
            if let Err(e) = write.send(Message::Text(req.to_string().into())).await {
                return Err(CrwError::RendererError(format!("WS send ({method}): {e}")));
            }
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(val))) => Ok(val),
            Ok(Ok(Err(msg))) => Err(CrwError::RendererError(format!("CDP {method}: {msg}"))),
            Ok(Err(_)) => Err(CrwError::RendererError(
                "CDP response channel dropped".into(),
            )),
            Err(_) => Err(CrwError::Timeout(timeout.as_millis() as u64)),
        }
    }

    /// Subscribe to the broadcast of all non-response (event) messages.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    /// Wait for an event that satisfies `pred`, or time out.
    pub async fn wait_for_event<F>(&self, mut pred: F, timeout: Duration) -> CrwResult<CdpEvent>
    where
        F: FnMut(&CdpEvent) -> bool,
    {
        let mut rx = self.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Err(_) => return Err(CrwError::Timeout(timeout.as_millis() as u64)),
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(CrwError::RendererError("event channel closed".into()));
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Ok(ev)) => {
                    if pred(&ev) {
                        return Ok(ev);
                    }
                }
            }
        }
    }

    /// Gracefully close the WebSocket and mark the connection unusable.
    pub async fn close(&self) {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut write = self.write.lock().await;
        let _ = tokio::time::timeout(WS_CLOSE_TIMEOUT, write.close()).await;
    }

    pub fn is_closed(&self) -> bool {
        self.is_closed.load(Ordering::SeqCst)
    }

    /// Liveness probe used by the browser-context pool when an idle slot has
    /// been parked longer than `health_check_secs`. Issues `Browser.getVersion`
    /// — a no-side-effect call that exercises both the WS write path and the
    /// reader loop. The 200 ms ceiling keeps acquire-path latency bounded.
    pub async fn health_check_browser(&self, timeout: Duration) -> CrwResult<()> {
        self.send_recv("Browser.getVersion", serde_json::json!({}), None, timeout)
            .await
            .map(|_| ())
    }
}

/// Removes a pending entry on drop. Ensures cancel-safety of `send_recv`:
/// if the caller's future is dropped while awaiting, the oneshot sender is
/// dropped from the map instead of leaking for the connection's lifetime.
struct PendingCleanup<'a> {
    pending: &'a DashMap<u64, oneshot::Sender<CdpResult>>,
    id: u64,
}

impl Drop for PendingCleanup<'_> {
    fn drop(&mut self) {
        self.pending.remove(&self.id);
    }
}

impl Drop for CdpConnection {
    fn drop(&mut self) {
        self.is_closed.store(true, Ordering::SeqCst);
        // Drain pending oneshot senders BEFORE aborting the event loop. If we
        // aborted first, every waiting `send_recv` would only learn about
        // closure when its channel was dropped — surfacing as a generic
        // "response channel dropped" rather than the real cause. Explicitly
        // delivering an error here gives callers a meaningful message even
        // though Drop can't `await` and the event loop's own drain pass will
        // never get a chance to run.
        //
        // Race with `dispatch`: the event loop may still be running and
        // calling `pending.remove(&id)` as we iterate here. That's fine —
        // `DashMap::remove` is atomic and returns an `Option`, so each `tx`
        // is consumed by exactly one path (dispatch's `Ok`, our `Err`, or
        // run_event_loop's exit drain). No double-send is possible, and
        // `let _ = tx.send(...)` is a no-op if the receiver already went away.
        let keys: Vec<u64> = self.pending.iter().map(|e| *e.key()).collect();
        for k in keys {
            if let Some((_, tx)) = self.pending.remove(&k) {
                let _ = tx.send(Err("CDP connection dropped".into()));
            }
        }
        if let Some(h) = self.event_loop.take() {
            h.abort();
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCdpMessage {
    id: Option<u64>,
    method: Option<String>,
    #[serde(default)]
    params: serde_json::Value,
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
    session_id: Option<String>,
}

/// Single-reader loop: routes responses back to their `oneshot::Sender`
/// (keyed by id) and broadcasts everything else as an event.
async fn run_event_loop(
    mut read: WsRead,
    pending: Arc<DashMap<u64, oneshot::Sender<CdpResult>>>,
    events: broadcast::Sender<CdpEvent>,
    is_closed: Arc<AtomicBool>,
) {
    while let Some(msg) = read.next().await {
        let text = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        if let Ok(raw) = serde_json::from_str::<RawCdpMessage>(&text) {
            dispatch(raw, &pending, &events);
        }
    }
    is_closed.store(true, Ordering::SeqCst);
    // Drain pending: nothing else will ever complete them.
    let keys: Vec<u64> = pending.iter().map(|e| *e.key()).collect();
    for k in keys {
        if let Some((_, tx)) = pending.remove(&k) {
            let _ = tx.send(Err("WS closed".into()));
        }
    }
}

fn dispatch(
    raw: RawCdpMessage,
    pending: &DashMap<u64, oneshot::Sender<CdpResult>>,
    events: &broadcast::Sender<CdpEvent>,
) {
    if let Some(id) = raw.id {
        if let Some((_, tx)) = pending.remove(&id) {
            let res = if let Some(err) = raw.error {
                Err(err.to_string())
            } else {
                Ok(raw.result.unwrap_or(serde_json::Value::Null))
            };
            let _ = tx.send(res);
        }
    } else if let Some(method) = raw.method {
        let _ = events.send(CdpEvent {
            method,
            params: raw.params,
            session_id: raw.session_id,
        });
    }
}

/// Params for `Target.createBrowserContext`, shared by both callers (the pool
/// and the legacy per-request proxy path).
///
/// `disposeOnDetach` is the load-bearing bit: targets created via
/// `Target.createTarget` are owned by the *browser*, not the CDP client, so
/// dropping the WebSocket leaves the renderer process alive forever. Every
/// explicit cleanup we do (`closeTarget` + `disposeBrowserContext`) is
/// best-effort and is skipped outright when the future is cancelled mid-fetch
/// — a timeout, a lost hedge race, a client disconnect — which is exactly when
/// cleanup matters most. With this flag Chrome disposes the context itself the
/// moment the session disconnects, closing every page in it without running
/// beforeunload. That makes the reap unconditional instead of best-effort.
///
/// Measured against prod Chrome 150 (2026-07-25): without the flag a dropped
/// socket left the renderer resident indefinitely; with it the renderer was
/// gone within seconds.
pub(crate) fn browser_ctx_params(proxy_server: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({ "disposeOnDetach": true });
    if let Some(p) = proxy_server {
        // No proxyBypassList: Chrome bypasses loopback by default, which is
        // what we want (don't route localhost via proxy).
        v["proxyServer"] = serde_json::Value::String(p.to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    /// Guards the leak fix: both createBrowserContext callers must ask Chrome
    /// to dispose the context on disconnect, with or without a proxy. Dropping
    /// this flag silently reintroduces the orphaned-renderer leak, which only
    /// shows up hours later as leaked chrome processes on the host.
    #[test]
    fn browser_ctx_params_always_dispose_on_detach() {
        let plain = browser_ctx_params(None);
        assert_eq!(plain["disposeOnDetach"], true);
        assert!(plain.get("proxyServer").is_none());

        let proxied = browser_ctx_params(Some("http://gw.example:823"));
        assert_eq!(proxied["disposeOnDetach"], true);
        assert_eq!(proxied["proxyServer"], "http://gw.example:823");
    }

    fn parse(json: &str) -> RawCdpMessage {
        serde_json::from_str(json).expect("valid RawCdpMessage")
    }

    fn host_header(ws_url: &str) -> String {
        let req = cdp_ws_request(ws_url).expect("request builds");
        req.headers()
            .get(HOST)
            .expect("Host header present")
            .to_str()
            .expect("Host is ascii")
            .to_string()
    }

    #[test]
    fn cdp_request_forces_localhost_host_for_a_bare_hostname() {
        // Chromium 148+ rejects `Host: chrome:9222`; `localhost:9222` passes.
        let req = cdp_ws_request("ws://chrome:9222/devtools/browser/x").expect("builds");
        assert_eq!(req.headers().get(HOST).unwrap(), "localhost:9222");
        // The URI keeps the hostname so TcpStream::connect resolves it and walks
        // every address the resolver returns (issue #564).
        assert_eq!(req.uri().host(), Some("chrome"));
        assert_eq!(req.uri().path(), "/devtools/browser/x");
    }

    #[test]
    fn cdp_request_keeps_default_host_for_ip_literals() {
        // The managed stack's static IP: already an IP literal, leave it alone.
        assert_eq!(
            host_header("ws://172.30.40.31:9222/devtools/browser/abc"),
            "172.30.40.31:9222"
        );
        // `Uri::host` keeps the brackets on v6, so the literal check must strip
        // them or a working `Host: [::1]:9222` would be replaced.
        assert_eq!(
            host_header("ws://[::1]:9222/devtools/browser/abc"),
            "[::1]:9222"
        );
        assert_eq!(
            host_header("ws://127.0.0.1:9222/devtools/browser/abc"),
            "127.0.0.1:9222"
        );
    }

    #[test]
    fn cdp_request_handles_the_browserless_shape() {
        // `config.stealth.toml` points the chrome tier at a plaintext browserless
        // endpoint on a published loopback port. `cdp.rs` skips discovery for a
        // `token=` URL, so this exact string reaches connect. Query and path must
        // survive untouched; only the Host is forced.
        let req = cdp_ws_request("ws://localhost:9224/chromium?token=crwtest&stealth=true")
            .expect("builds");
        assert_eq!(req.headers().get(HOST).unwrap(), "localhost:9224");
        assert_eq!(
            req.uri().path_and_query().map(|p| p.as_str()),
            Some("/chromium?token=crwtest&stealth=true")
        );
    }

    #[test]
    fn cdp_request_is_identical_to_the_default_for_an_ip_literal() {
        // The managed stack's chrome tier is an IP literal (`ws://172.30.40.31:9222/`).
        // It takes no override, so the request must match what `connect_async(&str)`
        // builds on its own, header for header.
        let url = "ws://172.30.40.31:9222/devtools/browser/abc";
        let ours = cdp_ws_request(url).expect("builds");
        let default = url.into_client_request().expect("builds");
        assert_eq!(ours.uri(), default.uri());
        assert_eq!(ours.method(), default.method());
        assert_eq!(ours.version(), default.version());
        let names: Vec<String> = ours.headers().keys().map(|k| k.to_string()).collect();
        let default_names: Vec<String> = default.headers().keys().map(|k| k.to_string()).collect();
        assert_eq!(names, default_names);
        for name in names.iter().filter(|n| n.as_str() != "sec-websocket-key") {
            assert_eq!(
                ours.headers().get(name),
                default.headers().get(name),
                "header {name} must be untouched"
            );
        }
    }

    #[test]
    fn cdp_request_leaves_wss_untouched() {
        // A hosted CDP endpoint needs its real Host, SNI and certificate name;
        // Chromium's guard does not apply to it.
        assert_eq!(
            host_header("wss://production-sfo.example.io/?token=abc"),
            "production-sfo.example.io"
        );
    }

    #[test]
    fn cdp_request_defaults_to_bare_localhost_without_a_port() {
        assert_eq!(host_header("ws://chrome/devtools/browser/x"), "localhost");
    }

    /// The repro for issue #564, in process. `localhost` resolves to both
    /// loopback families; the listener is bound to whichever one the resolver
    /// does NOT return first, so the first address always refuses. Before the
    /// fix the ws_url was pinned to that first address and the connect died
    /// there; keeping the hostname in the URI lets `TcpStream::connect` walk on
    /// to the second. Binding by resolver order rather than hardcoding
    /// `127.0.0.1` makes the test reproduce the bug on a v4-first host too.
    ///
    /// It also asserts the `Host` header actually sent, which is what
    /// Chromium's DNS-rebinding guard validates.
    #[tokio::test]
    async fn connects_through_localhost_when_the_first_resolved_address_refuses() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(("localhost", 0))
            .await
            .expect("resolve localhost")
            .collect();
        let Some(first) = addrs.first().copied() else {
            eprintln!("skipped: localhost resolved to nothing");
            return;
        };
        // Needs a second family to skip to; a single-stack host cannot show the bug.
        let Some(second) = addrs.iter().find(|a| a.is_ipv6() != first.is_ipv6()) else {
            eprintln!("skipped: localhost is single-stack, the bug cannot be shown here");
            return;
        };

        let listener = tokio::net::TcpListener::bind((second.ip(), 0))
            .await
            .expect("bind the second resolved family");
        let port = listener.local_addr().unwrap().port();

        // The premise is that the FIRST address refuses on this port. Ephemeral
        // ports are per-address, so another test in the suite could be holding
        // the same number on the other family; binding it proves it is free.
        match tokio::net::TcpListener::bind((first.ip(), port)).await {
            Ok(probe) => drop(probe),
            Err(_) => {
                eprintln!("skipped: port {port} is taken on the first resolved address");
                return;
            }
        }

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let mut host = String::new();
            let mut key = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.expect("read") == 0 {
                    break;
                }
                let Some((name, value)) = line.split_once(':') else {
                    if line == "\r\n" {
                        break;
                    }
                    continue;
                };
                // Header names are case-insensitive; the key's value is not.
                match name.to_ascii_lowercase().as_str() {
                    "host" => host = value.trim().to_string(),
                    "sec-websocket-key" => key = value.trim().to_string(),
                    _ => {}
                }
            }
            let accept =
                tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
            let resp = [
                "HTTP/1.1 101 Switching Protocols",
                "Upgrade: websocket",
                "Connection: Upgrade",
                &format!("Sec-WebSocket-Accept: {accept}"),
                "",
                "",
            ]
            .join("\r\n");
            reader
                .get_mut()
                .write_all(resp.as_bytes())
                .await
                .expect("write 101");
            host
        });

        let url = format!("ws://localhost:{port}/devtools/browser/x");
        let conn = CdpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect over ws://localhost past the refusing first address");

        let host = server.await.expect("server task");
        assert_eq!(host, format!("localhost:{port}"));
        drop(conn);
    }

    #[test]
    fn cdp_request_lowercases_the_scheme() {
        // `http::Uri` keeps `WS`; tungstenite matches only lowercase. Reachable
        // via the shapes `cdp.rs` passes through without discovery.
        let req = cdp_ws_request("WS://chrome:9222/devtools/browser/x").expect("builds");
        assert_eq!(req.uri().scheme_str(), Some("ws"));
        assert_eq!(req.headers().get(HOST).unwrap(), "localhost:9222");
        // Nothing but the scheme is touched.
        assert_eq!(req.uri().host(), Some("chrome"));
        assert_eq!(req.uri().path(), "/devtools/browser/x");
    }

    #[test]
    fn cdp_request_tolerates_surrounding_whitespace() {
        // A compose `.env` or TOML value can carry a stray space; `http::Uri`
        // rejects it outright where the previous `url::Url` parse did not.
        let req = cdp_ws_request("  ws://chrome:9222/devtools/browser/x\n").expect("builds");
        assert_eq!(req.uri().host(), Some("chrome"));
        assert_eq!(req.headers().get(HOST).unwrap(), "localhost:9222");
    }

    #[test]
    fn cdp_request_keeps_a_literal_localhost_host() {
        // Already `localhost`: the forced value is the same thing, with the port.
        assert_eq!(
            host_header("ws://localhost:9222/devtools/browser/x"),
            "localhost:9222"
        );
    }

    #[test]
    fn cdp_request_keeps_default_host_for_a_routable_ipv6_literal() {
        assert_eq!(
            host_header("ws://[2001:db8::1]:9222/devtools/browser/x"),
            "[2001:db8::1]:9222"
        );
    }

    #[test]
    fn cdp_request_rejects_a_malformed_url() {
        let err = cdp_ws_request("not a url").expect_err("malformed url is an error");
        // Sanitized: a category, never the URL itself.
        assert!(
            err.to_string()
                .starts_with("Renderer error: CDP connect failed: ")
        );
        assert!(!err.to_string().contains("not a url"));
    }

    #[tokio::test]
    async fn dispatch_routes_response_by_id() {
        let pending: DashMap<u64, oneshot::Sender<CdpResult>> = DashMap::new();
        let (events_tx, _rx) = broadcast::channel(16);
        let (tx, rx) = oneshot::channel::<CdpResult>();
        pending.insert(7, tx);

        dispatch(
            parse(r#"{"id":7,"result":{"value":42}}"#),
            &pending,
            &events_tx,
        );

        let got = rx.await.unwrap().unwrap();
        assert_eq!(got["value"], 42);
        assert!(pending.is_empty(), "pending entry consumed on delivery");
    }

    #[tokio::test]
    async fn dispatch_forwards_error_to_pending() {
        let pending: DashMap<u64, oneshot::Sender<CdpResult>> = DashMap::new();
        let (events_tx, _rx) = broadcast::channel(16);
        let (tx, rx) = oneshot::channel::<CdpResult>();
        pending.insert(1, tx);

        dispatch(
            parse(r#"{"id":1,"error":{"code":-32000,"message":"bad"}}"#),
            &pending,
            &events_tx,
        );

        let got = rx.await.unwrap();
        assert!(got.is_err());
        assert!(got.unwrap_err().contains("bad"));
    }

    #[tokio::test]
    async fn dispatch_broadcasts_event_without_id() {
        let pending: DashMap<u64, oneshot::Sender<CdpResult>> = DashMap::new();
        let (events_tx, mut rx) = broadcast::channel(16);

        dispatch(
            parse(
                r#"{"method":"Page.loadEventFired","params":{"timestamp":1.0},"sessionId":"s1"}"#,
            ),
            &pending,
            &events_tx,
        );

        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.method, "Page.loadEventFired");
        assert_eq!(ev.session_id.as_deref(), Some("s1"));
        assert_eq!(ev.params["timestamp"], 1.0);
    }

    #[tokio::test]
    async fn dispatch_drops_response_with_no_pending_entry() {
        // Late/duplicate response: must not panic, must not leak.
        let pending: DashMap<u64, oneshot::Sender<CdpResult>> = DashMap::new();
        let (events_tx, _rx) = broadcast::channel(16);
        dispatch(parse(r#"{"id":999,"result":{}}"#), &pending, &events_tx);
        assert!(pending.is_empty());
    }
}
