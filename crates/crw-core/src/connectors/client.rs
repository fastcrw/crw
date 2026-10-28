//! The HTTP client connector calls go out on. Dedicated, never shared with the
//! fastCRW API client, because every one of its rules is about a destination
//! the manifest chose:
//!
//! - a resolver that runs [`validate_resolved_ips`] on every answer, so a DNS
//!   rebind to a private address fails at connect time rather than after a
//!   check whose answer reqwest then throws away;
//! - [`validate_safe_url`] on the built URL and every redirect hop, since
//!   hyper skips DNS for IP-literal hosts;
//! - redirects followed by hand: https only, same origin only, at most 3;
//! - `no_proxy()`: the rebind guard lives in our resolver, and a proxy would
//!   resolve the name on its side of the wire;
//! - a streamed 8 MiB body cap.

use crate::url_safety::{validate_resolved_ips, validate_safe_url};
use futures::future::BoxFuture;
use reqwest::header::{HeaderMap, LOCATION};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub(crate) const MAX_REDIRECTS: usize = 3;
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(8);

/// Test-only relaxation: lets a test point a connector at a plain-http
/// loopback server. Production code always uses `Guard::default()`. The
/// resolver stays strict either way.
#[derive(Clone, Copy, Default)]
pub(crate) struct Guard {
    pub(crate) test_loopback: bool,
}

impl Guard {
    pub(crate) fn is_test_loopback(&self, url: &Url) -> bool {
        self.test_loopback
            && matches!(url.scheme(), "http" | "https")
            && match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            }
    }
}

/// A refusal raised inside the resolver, kept as its own type so the request
/// error can be told apart from an ordinary network failure.
#[derive(Debug)]
struct Blocked(String);

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Blocked {}

pub(crate) type Lookup =
    Arc<dyn Fn(String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> + Send + Sync>;

fn system_lookup() -> Lookup {
    Arc::new(|host: String| {
        Box::pin(async move {
            let addrs = tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host, 0)))
                .await
                .map_err(|_| std::io::Error::other("DNS resolution timed out"))??;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    })
}

pub(crate) struct GuardedResolver {
    lookup: Lookup,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = (self.lookup)(name.as_str().to_string());
        Box::pin(async move {
            let ips = lookup.await?;
            validate_resolved_ips(ips.iter().copied()).map_err(Blocked)?;
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

#[derive(Debug)]
pub(crate) enum FetchError {
    /// A destination rule refused the request or a hop.
    Blocked(String),
    Timeout,
    Network(String),
    TooLarge,
}

pub(crate) struct ConnectorHttp {
    client: reqwest::Client,
    guard: Guard,
}

impl ConnectorHttp {
    pub(crate) fn new(guard: Guard) -> Self {
        Self::with_lookup(guard, system_lookup())
    }

    pub(crate) fn with_lookup(guard: Guard, lookup: Lookup) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver { lookup }))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("connector http client builds");
        Self { client, guard }
    }

    pub(crate) fn guard(&self) -> Guard {
        self.guard
    }

    /// Scheme, literal address and origin rules for one hop.
    pub(crate) fn check_hop(&self, url: &Url, origin: &url::Origin) -> Result<(), String> {
        if !self.guard.is_test_loopback(url) {
            if url.scheme() != "https" {
                return Err("only https is allowed".into());
            }
            validate_safe_url(url)?;
        }
        if url.origin() != *origin {
            return Err("the request left the source's origin".into());
        }
        Ok(())
    }

    /// GET `url`, following up to [`MAX_REDIRECTS`] same-origin redirects.
    /// Returns the final response and the URL it came from.
    pub(crate) async fn get(
        &self,
        url: Url,
        headers: &HeaderMap,
    ) -> Result<(reqwest::Response, Url), FetchError> {
        let origin = url.origin();
        let mut current = url;
        for hop in 0..=MAX_REDIRECTS {
            self.check_hop(&current, &origin)
                .map_err(FetchError::Blocked)?;
            let resp = self
                .client
                .get(current.clone())
                .headers(headers.clone())
                .send()
                .await
                .map_err(classify)?;
            let location = resp
                .status()
                .is_redirection()
                .then(|| resp.headers().get(LOCATION).cloned())
                .flatten();
            let Some(location) = location else {
                return Ok((resp, current));
            };
            if hop == MAX_REDIRECTS {
                return Err(FetchError::Blocked("too many redirects".into()));
            }
            current = location
                .to_str()
                .ok()
                .and_then(|l| current.join(l).ok())
                .ok_or_else(|| FetchError::Blocked("unreadable redirect".into()))?;
        }
        unreachable!("the loop returns on its last hop")
    }
}

fn classify(e: reqwest::Error) -> FetchError {
    if e.is_timeout() {
        return FetchError::Timeout;
    }
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        if let Some(blocked) = cause.downcast_ref::<Blocked>() {
            return FetchError::Blocked(blocked.0.clone());
        }
        source = cause.source();
    }
    FetchError::Network(crate::error::reqwest_error_chain(e))
}

/// Read the body, stopping as soon as it passes [`MAX_BODY_BYTES`].
pub(crate) async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_BODY_BYTES as u64)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(classify)? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn switching_lookup(answers: Vec<Vec<IpAddr>>) -> Lookup {
        let calls = Arc::new(AtomicUsize::new(0));
        Arc::new(move |_host| {
            let i = calls.fetch_add(1, Ordering::SeqCst);
            let ips = answers[i.min(answers.len() - 1)].clone();
            Box::pin(async move { Ok(ips) })
        })
    }

    #[tokio::test]
    async fn connectors_resolver_refuses_a_rebind_to_loopback() {
        use reqwest::dns::Resolve;
        let resolver = GuardedResolver {
            lookup: switching_lookup(vec![
                vec!["93.184.216.34".parse().unwrap()],
                vec!["127.0.0.1".parse().unwrap()],
            ]),
        };
        let first = resolver.resolve("api.example.org".parse().unwrap()).await;
        assert!(first.is_ok(), "public answer must pass");
        let second = resolver.resolve("api.example.org".parse().unwrap()).await;
        let err = second.err().expect("loopback answer must be refused");
        assert!(err.to_string().contains("not allowed"), "{err}");
    }

    #[tokio::test]
    async fn connectors_request_to_a_name_resolving_to_loopback_never_connects() {
        let server = wiremock::MockServer::start().await;
        let port = server.address().port();
        // Strict guard; the name resolves to the live test server.
        let http = ConnectorHttp::with_lookup(
            Guard::default(),
            switching_lookup(vec![vec!["127.0.0.1".parse().unwrap()]]),
        );
        let url = Url::parse(&format!("https://rebind.example.org:{port}/x")).unwrap();
        let err = http.get(url, &HeaderMap::new()).await.err().unwrap();
        assert!(matches!(err, FetchError::Blocked(_)), "{err:?}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn connectors_hop_rules() {
        let http = ConnectorHttp::new(Guard::default());
        let origin = Url::parse("https://api.example.org").unwrap().origin();
        let ok = Url::parse("https://api.example.org/x").unwrap();
        assert!(http.check_hop(&ok, &origin).is_ok());
        for bad in [
            "http://api.example.org/x",
            "https://other.example.org/x",
            "https://10.0.0.1/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "https://169.254.169.254/latest",
        ] {
            let url = Url::parse(bad).unwrap();
            assert!(
                http.check_hop(&url, &origin).is_err(),
                "{bad} must be refused"
            );
        }
    }
}
