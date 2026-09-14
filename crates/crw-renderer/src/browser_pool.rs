//! Browser context pool — long-lived CDP connections + ephemeral browser
//! contexts amortizing per-request handshake (~200–500 ms) to ~0 ms on warm
//! hits. See `plans/tamam-browser-ppolu-detaylica-virtual-glade.md` for the
//! full design rationale; this module implements §"Design — ownership model".
//!
//! Lock discipline (load-bearing): the per-slot mutex is `std::sync::Mutex`
//! and is **never held across an `.await`**. Any change that violates that
//! invariant breaks both shutdown's force-close path and the sync `Drop`
//! emergency reaper. Audited at PR review time.
//!
//! Stealth JS injection and `Fetch.enable` are **strictly per-target session-
//! scoped**: do NOT hoist them to the connection or the browser-context level.
//! See `cdp.rs` for the per-target attach + setup sequence.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crw_core::error::{CrwError, CrwResult};

use crate::cdp_conn::CdpConnection;

/// Operations the pool performs against a Chrome connection. Trait-extracted
/// so unit tests can substitute a fake without bringing up real CDP. The blanket
/// impl below wires it to `CdpConnection`.
#[async_trait]
pub trait ChromeConnOps: Send + Sync + 'static {
    async fn create_browser_context(&self) -> CrwResult<String>;
    async fn dispose_browser_context(&self, ctx_id: &str) -> CrwResult<()>;
    async fn close_target(&self, target_id: &str) -> CrwResult<()>;
    async fn health_check(&self) -> CrwResult<()>;
    async fn close_conn(&self);
    fn is_closed(&self) -> bool;
}

#[async_trait]
impl ChromeConnOps for CdpConnection {
    async fn create_browser_context(&self) -> CrwResult<String> {
        let v = self
            .send_recv(
                "Target.createBrowserContext",
                crate::cdp_conn::browser_ctx_params(None),
                None,
                Duration::from_secs(2),
            )
            .await?;
        v.get("browserContextId")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrwError::RendererError(
                    "Target.createBrowserContext: missing browserContextId".into(),
                )
            })
    }

    async fn dispose_browser_context(&self, ctx_id: &str) -> CrwResult<()> {
        self.send_recv(
            "Target.disposeBrowserContext",
            json!({ "browserContextId": ctx_id }),
            None,
            Duration::from_secs(1),
        )
        .await
        .map(|_| ())
    }

    async fn close_target(&self, target_id: &str) -> CrwResult<()> {
        self.send_recv(
            "Target.closeTarget",
            json!({ "targetId": target_id }),
            None,
            Duration::from_secs(2),
        )
        .await
        .map(|_| ())
    }

    async fn health_check(&self) -> CrwResult<()> {
        CdpConnection::health_check_browser(self, Duration::from_millis(200)).await
    }

    async fn close_conn(&self) {
        CdpConnection::close(self).await;
    }

    fn is_closed(&self) -> bool {
        CdpConnection::is_closed(self)
    }
}

/// Per-phase recycle progress marker. Drives the `pool_recycle_seconds{phase}`
/// histogram and lets shutdown reason about where a Recycling slot is parked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecyclePhase {
    CloseTarget,
    DisposeCtx,
    CreateCtx,
}

/// Slot state machine. Transitions are serialized under the per-slot
/// `StdMutex<PooledSlot>` and held only synchronously.
pub enum SlotState<C: ChromeConnOps> {
    Idle {
        conn: Arc<C>,
        ctx_id: String,
    },
    /// `target_id` starts as `None` on every checkout. `record_target` (called
    /// synchronously inside `fetch_inner` immediately after `Target.createTarget`
    /// returns Ok, before any subsequent `.await`) flips it to `Some`. There
    /// are three legitimate ways `target_id` can still be `None` at release
    /// time: (a) `fetch_inner` failed at `createTarget` so the recorder never
    /// fired; (b) `record_target` lost a theoretical shutdown race and
    /// log-and-returned per its panic-free contract; (c) emergency `Drop` ran
    /// without ever entering `fetch_inner`. In all three cases `release()`
    /// skips `closeTarget` and still disposes+recreates ctx.
    CheckedOut {
        conn: Arc<C>,
        ctx_id: String,
        target_id: Option<String>,
    },
    Recycling {
        conn: Arc<C>,
        ctx_id: String,
        target_id: Option<String>,
        phase: RecyclePhase,
    },
    /// Conn ownership is explicit: `Some(_)` means whoever transitioned to
    /// `Dead` is responsible for closing it (release runs inline; Drop spawns
    /// a reaper; shutdown takes it via `take_conn()`). After close, conn
    /// becomes `None`.
    Dead {
        conn: Option<Arc<C>>,
    },
    /// Shutdown owns the close: phase 2 (clean idle drain) or phase 3 (force).
    Closing,
}

pub struct PooledSlot<C: ChromeConnOps> {
    pub state: SlotState<C>,
    /// Single-shot terminator: only ONE of {release, drop, shutdown-force-close}
    /// may consume this. `compare_exchange(false→true, AcqRel)` decides the
    /// winner; the loser does nothing (no decrement, no notify, no close).
    /// Reset to `false` on every `Idle → CheckedOut` transition under the slot
    /// mutex (the only legitimate `true → false` transition pool-wide).
    pub terminator: AtomicBool,
    pub last_used: Instant,
}

impl<C: ChromeConnOps> PooledSlot<C> {
    /// Single API for transferring conn ownership across paths. Always called
    /// under the slot mutex (caller holds the `MutexGuard` and `DerefMut`s to
    /// `&mut PooledSlot`). Idempotent: a second call always returns `None`.
    ///
    /// State matrix:
    ///
    /// | Current state                  | Return        | New state                    |
    /// |--------------------------------|---------------|------------------------------|
    /// | `Idle { conn, .. }`            | `Some(conn)`  | `Closing`                    |
    /// | `CheckedOut { conn, .. }`      | `Some(conn)`  | `Closing`                    |
    /// | `Recycling { conn, .. }`       | `Some(conn)`  | `Closing`                    |
    /// | `Dead { conn: Some(c) }`       | `Some(c)`     | `Dead { conn: None }`        |
    /// | `Dead { conn: None }`          | `None`        | `Dead { conn: None }`        |
    /// | `Closing`                      | `None`        | `Closing`                    |
    pub fn take_conn(&mut self) -> Option<Arc<C>> {
        let cur = std::mem::replace(&mut self.state, SlotState::Closing);
        match cur {
            SlotState::Idle { conn, .. }
            | SlotState::CheckedOut { conn, .. }
            | SlotState::Recycling { conn, .. } => {
                // state already replaced with Closing
                Some(conn)
            }
            SlotState::Dead { conn: Some(c) } => {
                self.state = SlotState::Dead { conn: None };
                Some(c)
            }
            SlotState::Dead { conn: None } => {
                self.state = SlotState::Dead { conn: None };
                None
            }
            SlotState::Closing => {
                self.state = SlotState::Closing;
                None
            }
        }
    }
}

pub type Slot<C> = Arc<StdMutex<PooledSlot<C>>>;
pub type WeakSlot<C> = Weak<StdMutex<PooledSlot<C>>>;

/// Async connection factory. Returns a fully-connected `Arc<C>`. Reuses
/// `cdp::connect_with_retry`-equivalent path so the cached-WS-URL invalidation
/// from commit `b5f7bec` is preserved (do NOT bypass).
pub type ConnFactory<C> =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = CrwResult<Arc<C>>> + Send>> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct PoolCfg {
    pub size: usize,
    /// Interactive render reserve override (`None` → `pool/4`). Carried from
    /// `[renderer.chrome_pool].reserved_interactive_renders`; resolved into the
    /// pool `BatchGate` at construction.
    pub reserved_interactive_renders: Option<usize>,
    pub recycle_after_navs: u32,
    pub idle_timeout: Duration,
    pub health_check_after: Duration,
    pub shutdown_drain: Duration,
    pub close_target_timeout: Duration,
    pub dispose_ctx_timeout: Duration,
    pub create_ctx_timeout: Duration,
}

impl Default for PoolCfg {
    fn default() -> Self {
        Self {
            size: 4,
            reserved_interactive_renders: None,
            recycle_after_navs: 1,
            idle_timeout: Duration::from_secs(300),
            health_check_after: Duration::from_secs(60),
            shutdown_drain: Duration::from_secs(30),
            close_target_timeout: Duration::from_secs(2),
            dispose_ctx_timeout: Duration::from_secs(1),
            create_ctx_timeout: Duration::from_secs(1),
        }
    }
}

pub struct BrowserContextPool<C: ChromeConnOps> {
    sem: Arc<Semaphore>,
    idle: Arc<StdMutex<VecDeque<Slot<C>>>>,
    all_slots: Arc<StdMutex<Vec<WeakSlot<C>>>>,
    inflight: Arc<AtomicUsize>,
    conn_factory: ConnFactory<C>,
    cfg: PoolCfg,
    closed: Arc<AtomicBool>,
    notify_idle: Arc<Notify>,
}

impl<C: ChromeConnOps> BrowserContextPool<C> {
    pub fn new(cfg: PoolCfg, conn_factory: ConnFactory<C>) -> Arc<Self> {
        let sem = Arc::new(Semaphore::new(cfg.size));
        Arc::new(Self {
            sem,
            idle: Arc::new(StdMutex::new(VecDeque::new())),
            all_slots: Arc::new(StdMutex::new(Vec::new())),
            inflight: Arc::new(AtomicUsize::new(0)),
            conn_factory,
            cfg,
            closed: Arc::new(AtomicBool::new(false)),
            notify_idle: Arc::new(Notify::new()),
        })
    }

    pub fn cfg(&self) -> &PoolCfg {
        &self.cfg
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::SeqCst)
    }

    pub fn idle_len(&self) -> usize {
        self.idle.lock().unwrap().len()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The single decrement+notify path. Called from `BookkeepingToken`
    /// (success and cancel paths) and directly from shutdown phase 3 when
    /// shutdown wins the terminator. Atomic ops + Notify only — never panics.
    fn dec_inflight_and_notify(&self) {
        // Saturating subtract: defensive against double-dec (which the
        // terminator AtomicBool already prevents). If a future bug ever
        // double-decs we want a stable 0 floor, not a wraparound to usize::MAX
        // that would make the shutdown drain loop spin forever.
        //
        // This must be a single atomic RMW. A `load`-then-`fetch_sub` pair is
        // check-then-act: two callers that both observe `prev == 1` would both
        // subtract and produce exactly the `usize::MAX` wraparound this guard
        // exists to prevent.
        let _ = self
            .inflight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(v.saturating_sub(1))
            });
        self.notify_idle.notify_waiters();
        self.refresh_gauges();
    }

    /// Snapshot inflight + idle into Prometheus gauges. Cheap (two atomic
    /// loads + lock + len + two `set` calls). Call from every mutation site
    /// so `chrome_pool_inflight` / `chrome_pool_idle` track actual state.
    fn refresh_gauges(&self) {
        let inflight = self.inflight.load(Ordering::SeqCst) as i64;
        // Poison-tolerant: reached from `PoolGuard::drop` via
        // `dec_inflight_and_notify`, which must not panic during future
        // cancellation. A panicking `Drop` while already unwinding aborts.
        let idle = match self.idle.lock() {
            Ok(g) => g.len(),
            Err(p) => p.into_inner().len(),
        } as i64;
        crw_core::metrics::metrics()
            .chrome_pool_inflight
            .set(inflight);
        crw_core::metrics::metrics().chrome_pool_idle.set(idle);
    }

    /// Walk `all_slots` and drop entries whose `Weak` is dead. Called on every
    /// new slot creation in `acquire()` (cold path) to bound the registry.
    fn prune_all_slots(&self) {
        let mut g = self.all_slots.lock().unwrap();
        g.retain(|w| w.strong_count() > 0);
    }

    /// Acquire a checked-out slot. Permit is acquired BEFORE any idle/create
    /// decision so two concurrent acquirers cannot both observe capacity and
    /// double-create. Permit is the source of truth for "you may exist in
    /// CheckedOut state".
    pub async fn acquire(self: &Arc<Self>) -> CrwResult<PoolGuard<C>> {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CrwError::Shutdown)?;

        // Two health-check retries max (plan §acquire flow step 3)
        for attempt in 0..3 {
            // Try popping an idle slot first
            let popped = self.idle.lock().unwrap().pop_front();
            let slot = if let Some(s) = popped {
                // Health-check if stale
                let needs_check = {
                    let g = s.lock().unwrap();
                    Instant::now().duration_since(g.last_used) > self.cfg.health_check_after
                };
                if needs_check {
                    // Snapshot conn for the async health check (no lock across await).
                    // The guard MUST be released before `mark_slot_dead_and_drop`,
                    // which re-locks the same non-reentrant `StdMutex`. A temporary
                    // in a `match` scrutinee lives until the end of the whole `match`,
                    // so taking the snapshot in its own block is load-bearing, not
                    // style: the `_` arm would otherwise self-deadlock.
                    let conn_opt = {
                        let g = s.lock().unwrap();
                        match &g.state {
                            SlotState::Idle { conn, .. } => Some(conn.clone()),
                            _ => None,
                        }
                    };
                    let Some(conn) = conn_opt else {
                        // Slot was somehow not Idle. Treat it as dead defensively.
                        self.mark_slot_dead_and_drop(&s);
                        continue;
                    };
                    if conn.health_check().await.is_err() {
                        self.mark_slot_dead_and_drop(&s);
                        if attempt < 2 {
                            continue;
                        }
                        return Err(CrwError::RendererError(
                            "pool: health check failed after retries".into(),
                        ));
                    }
                }
                s
            } else {
                // No idle slot — create a fresh one under the held permit
                let conn = (self.conn_factory)().await?;
                let ctx_id = conn.create_browser_context().await?;
                let slot = Arc::new(StdMutex::new(PooledSlot {
                    state: SlotState::Idle { conn, ctx_id },
                    terminator: AtomicBool::new(false),
                    last_used: Instant::now(),
                }));
                self.prune_all_slots();
                self.all_slots.lock().unwrap().push(Arc::downgrade(&slot));
                slot
            };

            // Transition Idle → CheckedOut under the slot mutex.
            // No `.await` between these atomic+sync operations.
            let (conn_clone, ctx_id_clone) = {
                let mut g = slot.lock().unwrap();
                // Verify state is Idle (sanity — only Idle slots are in the free-list)
                let (conn, ctx_id) = match std::mem::replace(&mut g.state, SlotState::Closing) {
                    SlotState::Idle { conn, ctx_id } => (conn, ctx_id),
                    other => {
                        // Race with shutdown phase 3 between pop and lock —
                        // restore state and retry
                        g.state = other;
                        if attempt < 2 {
                            drop(g);
                            continue;
                        }
                        return Err(CrwError::Shutdown);
                    }
                };
                // Reset terminator (only legitimate true→false transition)
                g.terminator.store(false, Ordering::SeqCst);
                let conn_clone = conn.clone();
                let ctx_id_clone = ctx_id.clone();
                g.state = SlotState::CheckedOut {
                    conn,
                    ctx_id,
                    target_id: None,
                };
                g.last_used = Instant::now();
                (conn_clone, ctx_id_clone)
            };

            self.inflight.fetch_add(1, Ordering::SeqCst);
            self.refresh_gauges();
            return Ok(PoolGuard {
                slot: Some(slot),
                permit: Some(permit),
                conn: conn_clone,
                ctx_id: ctx_id_clone,
                pool: Arc::downgrade(self),
            });
        }
        Err(CrwError::Internal("pool: acquire exhausted retries".into()))
    }

    /// Sync helper: transition slot to Dead{Some(conn)} if it currently holds a
    /// conn. Used on health-check failure (caller still holds the permit).
    fn mark_slot_dead_and_drop(&self, slot: &Slot<C>) {
        let conn_opt = {
            let mut g = slot.lock().unwrap();
            g.take_conn()
        };
        if let Some(conn) = conn_opt {
            // Best-effort close; do not block acquire path on it
            let conn_clone = conn.clone();
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1), conn_clone.close_conn()).await;
                });
            }
        }
    }

    /// Graceful shutdown. Three phases: (1) signal close + wait for inflight
    /// to drain via Notify, (2) dispose+close every Idle slot, (3) force-close
    /// anything still held in `all_slots` past the deadline AND reconcile its
    /// inflight bookkeeping. Post-condition: `inflight==0`, `idle.is_empty()`,
    /// every slot ∈ {Closing, Dead{None}}.
    pub async fn shutdown(self: Arc<Self>, drain: Duration) {
        self.closed.store(true, Ordering::SeqCst);
        self.sem.close();
        let deadline = Instant::now() + drain;

        // Phase 1: graceful — wait for inflight to drain via Notify
        while self.inflight.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = tokio::time::timeout(remaining, self.notify_idle.notified()).await;
        }

        // Phase 2: drain whatever's idle now (released cleanly during phase 1)
        let drained: Vec<Slot<C>> = {
            let mut g = self.idle.lock().unwrap();
            std::mem::take(&mut *g).into()
        };
        for slot in drained {
            // For Idle slots, we own the conn directly; dispose ctx then close
            let (ctx_opt, conn_opt) = {
                let mut g = slot.lock().unwrap();
                let ctx = match &g.state {
                    SlotState::Idle { ctx_id, .. } => Some(ctx_id.clone()),
                    _ => None,
                };
                (ctx, g.take_conn())
            };
            if let (Some(ctx), Some(conn)) = (ctx_opt.as_ref(), conn_opt.as_ref()) {
                let _ = tokio::time::timeout(
                    self.cfg.dispose_ctx_timeout,
                    conn.dispose_browser_context(ctx),
                )
                .await;
            }
            if let Some(conn) = conn_opt {
                let _ = tokio::time::timeout(Duration::from_secs(1), conn.close_conn()).await;
            }
        }

        // Phase 3: FORCE close anything still held. Guards that never returned
        // (leaked or cancelled mid-recycle) get their conns closed AND their
        // inflight reconciled here so the pool reaches inflight==0.
        let registry: Vec<WeakSlot<C>> = {
            let mut g = self.all_slots.lock().unwrap();
            g.retain(|w| w.strong_count() > 0);
            g.clone()
        };
        for weak in registry {
            let Some(slot) = weak.upgrade() else { continue };
            let we_own_bookkeeping = slot
                .lock()
                .unwrap()
                .terminator
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            let conn_opt = slot.lock().unwrap().take_conn();
            if let Some(conn) = conn_opt {
                let _ = tokio::time::timeout(Duration::from_secs(1), conn.close_conn()).await;
            }
            if we_own_bookkeeping {
                // CheckedOut/Recycling guard never returned: decrement inflight
                self.dec_inflight_and_notify();
            }
        }

        // Phase 4 (I1 fix): defensive idle-deque sweep. Phases 2 + 3 perform
        // async work between the `std::mem::take(&idle)` snapshot and the final
        // post-condition check; a release future that lost the terminator race
        // can still legally complete its happy path (state Recycling → Idle +
        // push_back) during those awaits and leave a stale Arc in the deque.
        // The slot's state is already terminal (Closing or Dead{None}) by now,
        // since phase 3 either claimed the terminator or observed release's
        // claim — so the deque entry is just a dangling Arc. Clearing it
        // satisfies the documented post-condition `idle.is_empty()` without
        // double-disposing.
        self.idle.lock().unwrap().clear();
        self.refresh_gauges();
    }
}

/// RAII bookkeeping handle. Created after release wins the terminator CAS;
/// consumed via `commit_decrement()` on normal exit. If the holding future is
/// cancelled before reaching `commit_decrement()`, `Drop` runs synchronously
/// and performs the decrement — closes the cancellation-mid-recycle gap.
///
/// Panic-free invariant: `Drop` runs during future cancellation in tokio.
/// MUST NOT panic — no unwrap, no debug_assert!. Internal helper only does
/// atomic ops + Notify.
#[must_use = "must call commit_decrement() on normal exit"]
pub(crate) struct BookkeepingToken<C: ChromeConnOps> {
    pool: Weak<BrowserContextPool<C>>,
    consumed: bool,
}

impl<C: ChromeConnOps> BookkeepingToken<C> {
    fn new(pool: Weak<BrowserContextPool<C>>) -> Self {
        Self {
            pool,
            consumed: false,
        }
    }

    fn commit_decrement(mut self) {
        if let Some(p) = self.pool.upgrade() {
            p.dec_inflight_and_notify();
        }
        self.consumed = true;
    }
}

impl<C: ChromeConnOps> Drop for BookkeepingToken<C> {
    fn drop(&mut self) {
        if !self.consumed
            && let Some(p) = self.pool.upgrade()
        {
            p.dec_inflight_and_notify();
            // pool already dropped → process is exiting, inflight irrelevant
        }
    }
}

pub struct PoolGuard<C: ChromeConnOps> {
    slot: Option<Slot<C>>,
    permit: Option<OwnedSemaphorePermit>,
    pub conn: Arc<C>,
    pub ctx_id: String,
    pool: Weak<BrowserContextPool<C>>,
}

impl<C: ChromeConnOps> PoolGuard<C> {
    /// Called synchronously inside `fetch_inner` immediately after
    /// `Target.createTarget` returns Ok, before any subsequent `.await`.
    /// Writes `Some(target_id)` into the slot's `CheckedOut` payload under the
    /// slot mutex.
    ///
    /// Panic-free contract: if the slot is no longer in `CheckedOut` state by
    /// the time this method acquires the slot mutex (theoretical shutdown
    /// race), log at WARN and return without mutating. Never panic.
    pub fn record_target(&self, target_id: String) {
        let Some(slot) = self.slot.as_ref() else {
            tracing::warn!("PoolGuard::record_target called without slot — bug");
            return;
        };
        let mut g = match slot.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // poison-tolerant: still try to record
        };
        if let SlotState::CheckedOut {
            target_id: ref mut tid,
            ..
        } = g.state
        {
            *tid = Some(target_id);
        } else {
            tracing::warn!("record_target: slot no longer CheckedOut (shutdown race) — skipping");
        }
    }

    /// Normal release path. Inline (no `tokio::spawn`). Sub-steps follow lock
    /// discipline + terminator AtomicBool gate + RAII `BookkeepingToken`.
    pub async fn release(mut self) -> CrwResult<()> {
        let slot = self
            .slot
            .take()
            .ok_or_else(|| CrwError::Internal("PoolGuard::release: slot already taken".into()))?;
        let permit = self.permit.take(); // dropped at function exit

        // Step 1: claim terminator. If shutdown already won, skip the rest.
        let we_won = slot
            .lock()
            .unwrap()
            .terminator
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !we_won {
            // Shutdown already claimed — drop permit and exit; bookkeeping is
            // shutdown's job now.
            drop(permit);
            return Ok(());
        }

        // Step 2: construct BookkeepingToken — from here on, cancellation is safe.
        let token = BookkeepingToken::new(self.pool.clone());

        // Step 3: lock slot, verify CheckedOut, snapshot, transition → Recycling.
        let (conn, ctx_id, target_id_opt) = {
            let mut g = slot.lock().unwrap();
            match std::mem::replace(&mut g.state, SlotState::Closing) {
                SlotState::CheckedOut {
                    conn,
                    ctx_id,
                    target_id,
                } => {
                    let conn_c = conn.clone();
                    let ctx_c = ctx_id.clone();
                    let tid = target_id.clone();
                    g.state = SlotState::Recycling {
                        conn,
                        ctx_id,
                        target_id,
                        phase: RecyclePhase::CloseTarget,
                    };
                    (conn_c, ctx_c, tid)
                }
                other => {
                    g.state = other;
                    // shutdown-raced abort path
                    record_recycle_outcome("shutdown_raced");
                    token.commit_decrement();
                    drop(permit);
                    return Ok(());
                }
            }
        };

        let pool = match self.pool.upgrade() {
            Some(p) => p,
            None => {
                // Pool dropped — process is exiting. Best-effort close on conn.
                let _ = tokio::time::timeout(Duration::from_secs(1), conn.close_conn()).await;
                token.commit_decrement();
                drop(permit);
                return Ok(());
            }
        };
        let metrics = crw_core::metrics::metrics();

        // Step 4: closeTarget (skip if target_id is None)
        if let Some(tid) = target_id_opt.as_deref() {
            let timer = metrics
                .chrome_pool_recycle_seconds
                .with_label_values(&["close_target"])
                .start_timer();
            let res =
                tokio::time::timeout(pool.cfg.close_target_timeout, conn.close_target(tid)).await;
            timer.observe_duration();
            match res {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    // closeTarget failed or timed out — treat conn as suspect.
                    metrics
                        .chrome_pool_recycle_failures_total
                        .with_label_values(&["close_target_timeout"])
                        .inc();
                    return Self::transition_to_dead_and_close(
                        &slot,
                        &conn,
                        "dead_conn",
                        token,
                        permit,
                    )
                    .await;
                }
            }
        }

        // Reconcile-after-close
        if !Self::reconcile_set_phase(&slot, RecyclePhase::DisposeCtx) {
            return Self::handle_shutdown_raced(token, permit);
        }

        // Step 5a: disposeBrowserContext
        let timer = metrics
            .chrome_pool_recycle_seconds
            .with_label_values(&["dispose_ctx"])
            .start_timer();
        let res = tokio::time::timeout(
            pool.cfg.dispose_ctx_timeout,
            conn.dispose_browser_context(&ctx_id),
        )
        .await;
        timer.observe_duration();
        if matches!(res, Ok(Err(_)) | Err(_)) {
            metrics
                .chrome_pool_recycle_failures_total
                .with_label_values(&["dispose_ctx_fail"])
                .inc();
            return Self::transition_to_dead_and_close(&slot, &conn, "dead_conn", token, permit)
                .await;
        }

        if !Self::reconcile_set_phase(&slot, RecyclePhase::CreateCtx) {
            return Self::handle_shutdown_raced(token, permit);
        }

        // Step 5b: createBrowserContext (fresh ctx_id)
        let timer = metrics
            .chrome_pool_recycle_seconds
            .with_label_values(&["create_ctx"])
            .start_timer();
        let create_res =
            tokio::time::timeout(pool.cfg.create_ctx_timeout, conn.create_browser_context()).await;
        timer.observe_duration();
        let fresh_ctx = match create_res {
            Ok(Ok(id)) => id,
            Ok(Err(_)) | Err(_) => {
                metrics
                    .chrome_pool_recycle_failures_total
                    .with_label_values(&["create_ctx_fail"])
                    .inc();
                return Self::transition_to_dead_and_close(
                    &slot,
                    &conn,
                    "dead_conn",
                    token,
                    permit,
                )
                .await;
            }
        };

        // Final reconcile: relock; if state is still Recycling, transition →
        // Idle and push to free-list.
        let did_push = {
            let mut g = slot.lock().unwrap();
            match std::mem::replace(&mut g.state, SlotState::Closing) {
                SlotState::Recycling { conn, .. } => {
                    g.state = SlotState::Idle {
                        conn,
                        ctx_id: fresh_ctx,
                    };
                    g.last_used = Instant::now();
                    true
                }
                other => {
                    g.state = other;
                    false
                }
            }
        };

        if did_push {
            pool.idle.lock().unwrap().push_back(slot);
            pool.notify_idle.notify_waiters();
            record_recycle_outcome("success");
        } else {
            record_recycle_outcome("shutdown_raced");
        }
        pool.refresh_gauges();

        token.commit_decrement();
        drop(permit);
        Ok(())
    }

    /// Relock the slot; if state is still Recycling, advance to `next_phase`
    /// and return `true`. Otherwise return `false` (shutdown raced).
    fn reconcile_set_phase(slot: &Slot<C>, next_phase: RecyclePhase) -> bool {
        let mut g = slot.lock().unwrap();
        if let SlotState::Recycling { ref mut phase, .. } = g.state {
            *phase = next_phase;
            true
        } else {
            false
        }
    }

    /// Failure-branch close handoff: transition Recycling → Dead{Some(conn)},
    /// then take_conn back out and close on the moved value. If shutdown's
    /// take_conn beat us, our take returns None and we skip close entirely
    /// (no double-close).
    async fn transition_to_dead_and_close(
        slot: &Slot<C>,
        _conn_ref: &Arc<C>,
        outcome: &'static str,
        token: BookkeepingToken<C>,
        permit: Option<OwnedSemaphorePermit>,
    ) -> CrwResult<()> {
        // Transition Recycling → Dead{Some(conn)} (or observe shutdown raced)
        let we_have_conn = {
            let mut g = slot.lock().unwrap();
            match std::mem::replace(&mut g.state, SlotState::Closing) {
                SlotState::Recycling { conn, .. } => {
                    g.state = SlotState::Dead { conn: Some(conn) };
                    true
                }
                other => {
                    g.state = other;
                    false
                }
            }
        };
        if !we_have_conn {
            // Shutdown took over mid-failure-branch
            record_recycle_outcome("shutdown_raced");
            token.commit_decrement();
            drop(permit);
            return Ok(());
        }

        // Now take_conn back out and close
        let conn_to_close = slot.lock().unwrap().take_conn();
        if let Some(c) = conn_to_close {
            let _ = tokio::time::timeout(Duration::from_secs(1), c.close_conn()).await;
        }
        record_recycle_outcome(outcome);
        token.commit_decrement();
        drop(permit);
        Ok(())
    }

    fn handle_shutdown_raced(
        token: BookkeepingToken<C>,
        permit: Option<OwnedSemaphorePermit>,
    ) -> CrwResult<()> {
        record_recycle_outcome("shutdown_raced");
        token.commit_decrement();
        drop(permit);
        Ok(())
    }
}

fn record_recycle_outcome(outcome: &'static str) {
    crw_core::metrics::metrics()
        .chrome_pool_recycle_total
        .with_label_values(&[outcome])
        .inc();
}

impl<C: ChromeConnOps> Drop for PoolGuard<C> {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            // release() already took the slot — nothing to do here.
            return;
        };

        // Step 1: claim terminator.
        // Poison-tolerant throughout this Drop: per the panic-free invariant
        // above, unwrapping a poisoned slot mutex here would panic while
        // unwinding and abort the process. `record_target` already takes this
        // approach; `Drop` needs it strictly more.
        let we_won = slot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .terminator
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !we_won {
            // shutdown or release already won; permit drops below
            return;
        }

        // Step 2: lock slot, take conn out, transition → Dead{Some(conn)}.
        // Capture ctx_id + target_id as well: on cancellation we must explicitly
        // reap the browser-owned target/context (Step 5) — a bare connection
        // close does NOT free them, so dropping the ids here is what leaked the
        // renderer process on every timed-out / cancelled render.
        let cleanup = {
            let mut g = slot.lock().unwrap_or_else(|p| p.into_inner());
            match std::mem::replace(&mut g.state, SlotState::Closing) {
                SlotState::CheckedOut {
                    conn,
                    ctx_id,
                    target_id,
                }
                | SlotState::Recycling {
                    conn,
                    ctx_id,
                    target_id,
                    ..
                } => {
                    g.state = SlotState::Dead { conn: Some(conn) };
                    // immediate take to Dead{None}, paired with the ids to reap
                    g.take_conn().map(|c| (c, ctx_id, target_id))
                }
                other => {
                    g.state = other;
                    None
                }
            }
        };

        // Step 3: metrics
        record_recycle_outcome("emergency_drop");
        crw_core::metrics::metrics()
            .chrome_pool_recycle_failures_total
            .with_label_values(&["missed_release"])
            .inc();

        // Step 4: dec inflight
        if let Some(pool) = self.pool.upgrade() {
            pool.dec_inflight_and_notify();
        }

        // Step 5: best-effort reap of the browser-owned target + context BEFORE
        // closing the connection. Targets created via Target.createTarget are
        // owned by the browser (not the CDP client) and disposeOnDetach is not
        // set, so a bare WS/conn close leaves an orphaned renderer process.
        // Mirror release()'s reap (closeTarget → disposeBrowserContext), but
        // best-effort with timeouts on a detached task (Drop cannot await).
        if let Some((conn, ctx_id, target_id)) = cleanup
            && tokio::runtime::Handle::try_current().is_ok()
        {
            tokio::spawn(async move {
                if let Some(tid) = target_id.as_deref() {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(5), conn.close_target(tid)).await;
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    conn.dispose_browser_context(&ctx_id),
                )
                .await;
                let _ = tokio::time::timeout(Duration::from_secs(1), conn.close_conn()).await;
            });
        }
        // Step 6: permit drops with self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// Minimal fake connection — counts each CDP op for assertions.
    #[derive(Default)]
    struct FakeConn {
        create_ctx_calls: AtomicU32,
        dispose_ctx_calls: AtomicU32,
        close_target_calls: AtomicU32,
        close_conn_calls: AtomicU32,
        health_check_calls: AtomicU32,
        close_target_should_fail: AtomicBool,
        dispose_ctx_should_fail: AtomicBool,
        create_ctx_should_fail: AtomicBool,
        health_check_should_fail: AtomicBool,
        last_close_target_id: StdMutex<Option<String>>,
        last_dispose_ctx_id: StdMutex<Option<String>>,
        closed: AtomicBool,
    }

    #[async_trait]
    impl ChromeConnOps for FakeConn {
        async fn create_browser_context(&self) -> CrwResult<String> {
            let n = self.create_ctx_calls.fetch_add(1, Ordering::SeqCst);
            if self.create_ctx_should_fail.load(Ordering::SeqCst) {
                return Err(CrwError::RendererError("create_ctx forced fail".into()));
            }
            Ok(format!("ctx-{n}"))
        }
        async fn dispose_browser_context(&self, ctx_id: &str) -> CrwResult<()> {
            self.dispose_ctx_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_dispose_ctx_id.lock().unwrap() = Some(ctx_id.to_string());
            if self.dispose_ctx_should_fail.load(Ordering::SeqCst) {
                Err(CrwError::RendererError("dispose_ctx forced fail".into()))
            } else {
                Ok(())
            }
        }
        async fn close_target(&self, target_id: &str) -> CrwResult<()> {
            self.close_target_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_close_target_id.lock().unwrap() = Some(target_id.to_string());
            if self.close_target_should_fail.load(Ordering::SeqCst) {
                Err(CrwError::RendererError("close_target forced fail".into()))
            } else {
                Ok(())
            }
        }
        async fn health_check(&self) -> CrwResult<()> {
            self.health_check_calls.fetch_add(1, Ordering::SeqCst);
            if self.health_check_should_fail.load(Ordering::SeqCst) {
                Err(CrwError::RendererError("health check forced fail".into()))
            } else {
                Ok(())
            }
        }
        async fn close_conn(&self) {
            self.close_conn_calls.fetch_add(1, Ordering::SeqCst);
            self.closed.store(true, Ordering::SeqCst);
        }
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }
    }

    fn fake_factory() -> ConnFactory<FakeConn> {
        Arc::new(|| Box::pin(async { Ok(Arc::new(FakeConn::default())) }))
    }

    /// A factory whose FIRST call fails, then succeeds on every subsequent
    /// call. Used to verify a failed `acquire()` does not leak the semaphore
    /// permit it already holds.
    fn factory_failing_first_call() -> ConnFactory<FakeConn> {
        let first = Arc::new(AtomicBool::new(true));
        Arc::new(move || {
            let first = first.clone();
            Box::pin(async move {
                if first.swap(false, Ordering::SeqCst) {
                    Err(CrwError::RendererError("factory forced fail".into()))
                } else {
                    Ok(Arc::new(FakeConn::default()))
                }
            })
        })
    }

    /// A factory that always succeeds connecting, but whose FIRST produced
    /// connection fails `create_browser_context` (the ctx-creation step run
    /// right after connect, inside the same `acquire()` cold path).
    fn factory_first_conn_ctx_creation_fails() -> ConnFactory<FakeConn> {
        let first = Arc::new(AtomicBool::new(true));
        Arc::new(move || {
            let first = first.clone();
            Box::pin(async move {
                let conn = Arc::new(FakeConn::default());
                if first.swap(false, Ordering::SeqCst) {
                    conn.create_ctx_should_fail.store(true, Ordering::SeqCst);
                }
                Ok(conn)
            })
        })
    }

    /// Build a manually-crafted idle slot (bypassing `acquire()`'s normal
    /// creation path) so tests can exercise the health-check / eviction logic
    /// without waiting out real `health_check_after` durations.
    fn stale_idle_slot(conn: Arc<FakeConn>, ctx_id: &str, age: Duration) -> Slot<FakeConn> {
        Arc::new(StdMutex::new(PooledSlot {
            state: SlotState::Idle {
                conn,
                ctx_id: ctx_id.to_string(),
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now() - age,
        }))
    }

    fn small_pool_cfg() -> PoolCfg {
        PoolCfg {
            size: 2,
            reserved_interactive_renders: None,
            recycle_after_navs: 1,
            idle_timeout: Duration::from_secs(300),
            health_check_after: Duration::from_secs(60),
            shutdown_drain: Duration::from_secs(2),
            close_target_timeout: Duration::from_millis(500),
            dispose_ctx_timeout: Duration::from_millis(500),
            create_ctx_timeout: Duration::from_millis(500),
        }
    }

    #[tokio::test]
    async fn take_conn_state_matrix() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let mut slot = PooledSlot {
            state: SlotState::Idle {
                conn: conn.clone(),
                ctx_id: "x".into(),
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now(),
        };
        // Idle → returns Some, transitions to Closing
        assert!(slot.take_conn().is_some());
        assert!(matches!(slot.state, SlotState::Closing));
        // Closing → returns None, stays Closing
        assert!(slot.take_conn().is_none());
        assert!(matches!(slot.state, SlotState::Closing));

        // Dead{Some} → returns Some, transitions to Dead{None}
        slot.state = SlotState::Dead {
            conn: Some(conn.clone()),
        };
        assert!(slot.take_conn().is_some());
        assert!(matches!(slot.state, SlotState::Dead { conn: None }));
        // Dead{None} → returns None, stays Dead{None}
        assert!(slot.take_conn().is_none());
    }

    #[tokio::test]
    async fn acquire_release_happy_path() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        assert_eq!(pool.inflight(), 0);
        assert_eq!(pool.idle_len(), 0);

        let guard = pool.acquire().await.expect("acquire");
        assert_eq!(pool.inflight(), 1);
        assert_eq!(pool.idle_len(), 0);

        // Pretend fetch_inner created a target
        guard.record_target("t-1".into());
        let conn = guard.conn.clone();
        guard.release().await.expect("release");

        assert_eq!(pool.inflight(), 0);
        assert_eq!(pool.idle_len(), 1);
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 1);
        // create_ctx: once on creation + once on recycle
        assert_eq!(conn.create_ctx_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn drop_reaps_target_and_context() {
        // Regression: a render cancelled by the outer timeout drops PoolGuard
        // WITHOUT release(). Drop must still reap the browser-owned target +
        // context, not just close the connection — otherwise the renderer
        // process leaks (the 3-6 day-old orphans observed in prod).
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        guard.record_target("t-1".into());
        let conn = guard.conn.clone();

        drop(guard); // simulate cancellation: no release()

        // Drop reaps on a detached task; give it time to run.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            conn.close_target_calls.load(Ordering::SeqCst),
            1,
            "Drop must Target.closeTarget the orphaned target"
        );
        assert_eq!(
            conn.dispose_ctx_calls.load(Ordering::SeqCst),
            1,
            "Drop must disposeBrowserContext"
        );
        assert_eq!(
            conn.close_conn_calls.load(Ordering::SeqCst),
            1,
            "Drop must still close the connection last"
        );
        assert_eq!(pool.inflight(), 0, "Drop must decrement inflight");
    }

    #[tokio::test]
    async fn release_skips_close_target_when_none() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        // Do NOT call record_target — simulates createTarget failure
        guard.release().await.unwrap();
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 0);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 1);
        assert_eq!(pool.inflight(), 0);
        assert_eq!(pool.idle_len(), 1);
    }

    #[tokio::test]
    async fn slot_reuse_terminator_resets() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let g1 = pool.acquire().await.unwrap();
        g1.record_target("t-a".into());
        g1.release().await.unwrap();
        assert_eq!(pool.inflight(), 0);

        // Second acquire should reuse the recycled slot
        let g2 = pool.acquire().await.unwrap();
        assert_eq!(pool.inflight(), 1);
        // Verify terminator was reset to false (i.e. release won the CAS again)
        g2.record_target("t-b".into());
        g2.release().await.unwrap();
        assert_eq!(pool.inflight(), 0);
        assert_eq!(pool.idle_len(), 1);
    }

    #[tokio::test]
    async fn drop_without_release_marks_dead_and_decrements() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        assert_eq!(pool.inflight(), 1);
        drop(guard); // emergency Drop path
        // dec_inflight runs synchronously inside Drop
        assert_eq!(pool.inflight(), 0);
        // Idle list should NOT contain the dead slot
        assert_eq!(pool.idle_len(), 0);
    }

    #[tokio::test]
    async fn close_target_failure_marks_dead_and_skips_dispose() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        conn.close_target_should_fail.store(true, Ordering::SeqCst);
        guard.record_target("t-1".into());
        guard.release().await.unwrap();
        // close_target was called, failed
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 1);
        // dispose_ctx must NOT be called (suspect-conn policy)
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 0);
        // Conn closed
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
        // Slot did NOT return to idle
        assert_eq!(pool.idle_len(), 0);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_idle_slots() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let g = pool.acquire().await.unwrap();
        let conn = g.conn.clone();
        g.record_target("t-1".into());
        g.release().await.unwrap();
        assert_eq!(pool.idle_len(), 1);

        pool.clone().shutdown(Duration::from_secs(1)).await;
        assert!(pool.is_closed());
        // Shutdown should have closed the conn we held
        assert!(conn.close_conn_calls.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn shutdown_force_closes_leaked_guard() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        std::mem::forget(guard); // simulate leak — no Drop runs
        assert_eq!(pool.inflight(), 1);

        pool.clone().shutdown(Duration::from_millis(200)).await;
        // Phase 3 should have closed the leaked conn AND decremented inflight
        assert_eq!(pool.inflight(), 0);
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_refuses_new_acquires() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        pool.clone().shutdown(Duration::from_millis(100)).await;
        let r = pool.acquire().await;
        assert!(matches!(r, Err(CrwError::Shutdown)));
    }

    // ── PoolCfg defaults ─────────────────────────────────────────────

    #[test]
    fn default_pool_cfg_matches_documented_values() {
        let cfg = PoolCfg::default();
        assert_eq!(cfg.size, 4);
        assert_eq!(cfg.reserved_interactive_renders, None);
        assert_eq!(cfg.recycle_after_navs, 1);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(300));
        assert_eq!(cfg.health_check_after, Duration::from_secs(60));
        assert_eq!(cfg.shutdown_drain, Duration::from_secs(30));
        assert_eq!(cfg.close_target_timeout, Duration::from_secs(2));
        assert_eq!(cfg.dispose_ctx_timeout, Duration::from_secs(1));
        assert_eq!(cfg.create_ctx_timeout, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn cfg_accessor_returns_the_configured_pool_cfg() {
        let cfg = PoolCfg {
            size: 7,
            ..small_pool_cfg()
        };
        let pool = BrowserContextPool::new(cfg, fake_factory());
        assert_eq!(pool.cfg().size, 7);
    }

    #[tokio::test]
    async fn pool_cfg_reserved_interactive_renders_is_carried_through() {
        let cfg = PoolCfg {
            reserved_interactive_renders: Some(2),
            ..small_pool_cfg()
        };
        let pool = BrowserContextPool::new(cfg, fake_factory());
        assert_eq!(pool.cfg().reserved_interactive_renders, Some(2));
    }

    // ── size / exhaustion boundaries ────────────────────────────────

    #[tokio::test]
    async fn acquire_unblocks_after_release_frees_the_permit() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 1,
                ..small_pool_cfg()
            },
            fake_factory(),
        );
        let g1 = pool.acquire().await.unwrap();
        let pool2 = pool.clone();
        let handle = tokio::spawn(async move { pool2.acquire().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !handle.is_finished(),
            "acquire on an exhausted size=1 pool must block"
        );
        g1.release().await.unwrap();
        let g2 = handle
            .await
            .unwrap()
            .expect("second acquire should succeed once the permit frees");
        assert_eq!(pool.inflight(), 1);
        drop(g2);
    }

    #[tokio::test]
    async fn pool_size_zero_acquire_blocks_then_fails_on_shutdown() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 0,
                ..small_pool_cfg()
            },
            fake_factory(),
        );
        let pool2 = pool.clone();
        let handle = tokio::spawn(async move { pool2.acquire().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !handle.is_finished(),
            "acquire on a zero-size pool must block forever absent shutdown"
        );
        pool.clone().shutdown(Duration::from_millis(50)).await;
        let result = handle.await.unwrap();
        assert!(matches!(result, Err(CrwError::Shutdown)));
    }

    #[tokio::test]
    async fn concurrent_acquire_up_to_pool_size_both_succeed() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 2,
                ..small_pool_cfg()
            },
            fake_factory(),
        );
        let g1 = pool.acquire().await.unwrap();
        let g2 = pool.acquire().await.unwrap();
        assert_eq!(pool.inflight(), 2);
        // Not asserting distinct ctx ids here: the shared `fake_factory()` in
        // this module hands out a fixed id, so id uniqueness is a property of
        // the real factory, not something this fake can demonstrate.
        drop(g1);
        drop(g2);
    }

    // ── terminator race: concurrent cleanup must never double-act ────

    #[tokio::test]
    async fn release_is_a_noop_when_terminator_already_claimed() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        // Simulate a concurrent winner (e.g. shutdown's phase-3 force-close)
        // claiming the terminator before release() gets a chance to.
        guard
            .slot
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .terminator
            .store(true, Ordering::SeqCst);
        guard.release().await.unwrap();
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 0);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn drop_is_a_noop_when_terminator_already_claimed() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        guard
            .slot
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .terminator
            .store(true, Ordering::SeqCst);
        drop(guard);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            conn.close_conn_calls.load(Ordering::SeqCst),
            0,
            "Drop must not act when another path already won the terminator"
        );
    }

    // ── record_target edge cases ──────────────────────────────────────

    #[tokio::test]
    async fn record_target_without_a_slot_does_not_panic() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let guard = PoolGuard::<FakeConn> {
            slot: None,
            permit: None,
            conn,
            ctx_id: "ctx".into(),
            pool: Weak::new(),
        };
        guard.record_target("t-1".into()); // must log + return, never panic
    }

    #[tokio::test]
    async fn record_target_when_slot_not_checked_out_logs_and_does_not_panic() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        {
            let slot = guard.slot.as_ref().unwrap();
            let mut g = slot.lock().unwrap();
            let (conn, ctx_id) = match std::mem::replace(&mut g.state, SlotState::Closing) {
                SlotState::CheckedOut { conn, ctx_id, .. } => (conn, ctx_id),
                _ => unreachable!("freshly acquired guard is always CheckedOut"),
            };
            g.state = SlotState::Recycling {
                conn,
                ctx_id,
                target_id: None,
                phase: RecyclePhase::CloseTarget,
            };
        }
        guard.record_target("t-1".into()); // state mismatch — must not panic
    }

    #[tokio::test]
    async fn record_target_survives_a_poisoned_slot_mutex() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let slot = guard.slot.as_ref().unwrap().clone();

        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = slot.lock().unwrap();
            panic!("intentional poison for the test");
        }));
        assert!(poisoned.is_err());
        assert!(slot.is_poisoned());

        // The poison-tolerant `match slot.lock() { Err(p) => p.into_inner(), .. }`
        // path must still record instead of panicking.
        guard.record_target("t-1".into());
    }

    // ── health-check-driven eviction on acquire ───────────────────────

    #[tokio::test]
    async fn acquire_health_checks_stale_idle_slot_and_reuses_when_healthy() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let conn = Arc::new(FakeConn::default());
        let stale = stale_idle_slot(conn.clone(), "stale-ctx", Duration::from_secs(120));
        pool.idle.lock().unwrap().push_back(stale.clone());
        pool.all_slots.lock().unwrap().push(Arc::downgrade(&stale));

        let guard = pool.acquire().await.unwrap();
        assert_eq!(
            guard.ctx_id, "stale-ctx",
            "a healthy stale slot must be reused, not discarded"
        );
        assert!(Arc::ptr_eq(&guard.conn, &conn));
        assert_eq!(conn.health_check_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn health_check_not_invoked_when_slot_was_recently_used() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let conn = Arc::new(FakeConn::default());
        let fresh = stale_idle_slot(conn.clone(), "fresh-ctx", Duration::from_millis(0));
        pool.idle.lock().unwrap().push_back(fresh.clone());
        pool.all_slots.lock().unwrap().push(Arc::downgrade(&fresh));

        let guard = pool.acquire().await.unwrap();
        assert_eq!(guard.ctx_id, "fresh-ctx");
        assert_eq!(
            conn.health_check_calls.load(Ordering::SeqCst),
            0,
            "a recently-used slot must skip the health check entirely"
        );
    }

    #[tokio::test]
    async fn acquire_evicts_unhealthy_stale_slot_and_creates_a_fresh_one() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let dead_conn = Arc::new(FakeConn::default());
        dead_conn
            .health_check_should_fail
            .store(true, Ordering::SeqCst);
        let stale = stale_idle_slot(dead_conn.clone(), "stale-ctx", Duration::from_secs(120));
        pool.idle.lock().unwrap().push_back(stale.clone());
        pool.all_slots.lock().unwrap().push(Arc::downgrade(&stale));

        let guard = pool.acquire().await.unwrap();
        assert_ne!(
            guard.ctx_id, "stale-ctx",
            "an unhealthy stale slot must not be reused"
        );
        assert!(!Arc::ptr_eq(&guard.conn, &dead_conn));

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            dead_conn.close_conn_calls.load(Ordering::SeqCst) >= 1,
            "the evicted dead connection must be closed, not leaked"
        );
    }

    #[tokio::test]
    async fn acquire_returns_error_after_exhausting_health_check_retries() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        for i in 0..3 {
            let dead_conn = Arc::new(FakeConn::default());
            dead_conn
                .health_check_should_fail
                .store(true, Ordering::SeqCst);
            let stale = stale_idle_slot(dead_conn, &format!("stale-{i}"), Duration::from_secs(120));
            pool.idle.lock().unwrap().push_back(stale.clone());
            pool.all_slots.lock().unwrap().push(Arc::downgrade(&stale));
        }
        let result = pool.acquire().await;
        assert!(
            matches!(result, Err(CrwError::RendererError(_))),
            "3 consecutive unhealthy stale slots must exhaust the 3 retry attempts"
        );
    }

    #[tokio::test]
    async fn acquire_after_dead_marking_does_not_resurrect_the_dead_slot() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let dead_conn = guard.conn.clone();
        dead_conn
            .close_target_should_fail
            .store(true, Ordering::SeqCst);
        guard.record_target("t-1".into());
        guard.release().await.unwrap(); // recycle fails -> slot goes Dead, not Idle
        assert_eq!(pool.idle_len(), 0);

        let guard2 = pool.acquire().await.unwrap();
        assert!(
            !Arc::ptr_eq(&guard2.conn, &dead_conn),
            "acquire must create a fresh connection, never resurrect a Dead one"
        );
    }

    // ── recycle failure paths (dispose_ctx / create_ctx) ──────────────

    #[tokio::test]
    async fn release_dispose_ctx_failure_marks_dead_and_closes_conn() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        guard.record_target("t-1".into());
        conn.dispose_ctx_should_fail.store(true, Ordering::SeqCst);
        guard.release().await.unwrap();
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(pool.idle_len(), 0);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn release_create_ctx_failure_marks_dead_and_closes_conn() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        guard.record_target("t-1".into());
        conn.create_ctx_should_fail.store(true, Ordering::SeqCst);
        guard.release().await.unwrap();
        // close_target + dispose_ctx both succeeded; the recycle create_ctx failed.
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(pool.idle_len(), 0);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn close_target_receives_the_recorded_target_id() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        guard.record_target("t-42".into());
        guard.release().await.unwrap();
        assert_eq!(
            *conn.last_close_target_id.lock().unwrap(),
            Some("t-42".to_string())
        );
    }

    #[tokio::test]
    async fn dispose_ctx_receives_the_current_ctx_id() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        let ctx_id = guard.ctx_id.clone();
        guard.record_target("t-1".into());
        guard.release().await.unwrap();
        assert_eq!(*conn.last_dispose_ctx_id.lock().unwrap(), Some(ctx_id));
    }

    #[tokio::test]
    async fn drop_reap_is_skipped_when_target_id_was_never_recorded() {
        // Mirrors `release_skips_close_target_when_none` but for the Drop
        // (cancellation) path: createTarget never returned, so there is no
        // browser-owned target to reap — only the context.
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        drop(guard); // no record_target call
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(conn.close_target_calls.load(Ordering::SeqCst), 0);
        assert_eq!(conn.dispose_ctx_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
    }

    // ── permit leak guards on the acquire cold-path error branches ────

    #[tokio::test]
    async fn acquire_conn_factory_failure_releases_the_permit_for_retry() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 1,
                ..small_pool_cfg()
            },
            factory_failing_first_call(),
        );
        let first = pool.acquire().await;
        assert!(
            first.is_err(),
            "the factory's forced failure must propagate"
        );
        let second = pool.acquire().await;
        assert!(
            second.is_ok(),
            "a failed connect during acquire must not leak the semaphore permit"
        );
    }

    #[tokio::test]
    async fn acquire_create_ctx_failure_releases_the_permit_for_retry() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 1,
                ..small_pool_cfg()
            },
            factory_first_conn_ctx_creation_fails(),
        );
        let first = pool.acquire().await;
        assert!(first.is_err());
        let second = pool.acquire().await;
        assert!(
            second.is_ok(),
            "a failed create_browser_context during acquire must not leak the permit"
        );
    }

    // ── reconcile_set_phase (private recycle-phase helper) ────────────

    #[test]
    fn reconcile_set_phase_returns_true_and_advances_when_recycling() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let slot: Slot<FakeConn> = Arc::new(StdMutex::new(PooledSlot {
            state: SlotState::Recycling {
                conn,
                ctx_id: "ctx".into(),
                target_id: None,
                phase: RecyclePhase::CloseTarget,
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now(),
        }));
        let advanced = PoolGuard::reconcile_set_phase(&slot, RecyclePhase::DisposeCtx);
        assert!(advanced);
        let g = slot.lock().unwrap();
        assert!(matches!(
            g.state,
            SlotState::Recycling {
                phase: RecyclePhase::DisposeCtx,
                ..
            }
        ));
    }

    #[test]
    fn reconcile_set_phase_returns_false_when_not_recycling() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let slot: Slot<FakeConn> = Arc::new(StdMutex::new(PooledSlot {
            state: SlotState::Idle {
                conn,
                ctx_id: "ctx".into(),
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now(),
        }));
        assert!(!PoolGuard::reconcile_set_phase(
            &slot,
            RecyclePhase::CreateCtx
        ));
    }

    // ── take_conn state matrix (additional states) ────────────────────

    #[test]
    fn take_conn_from_checked_out_transitions_to_closing() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let mut slot = PooledSlot {
            state: SlotState::CheckedOut {
                conn,
                ctx_id: "x".into(),
                target_id: Some("t".into()),
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now(),
        };
        assert!(slot.take_conn().is_some());
        assert!(matches!(slot.state, SlotState::Closing));
        assert!(slot.take_conn().is_none());
    }

    #[test]
    fn take_conn_from_recycling_transitions_to_closing() {
        let conn: Arc<FakeConn> = Arc::new(FakeConn::default());
        let mut slot = PooledSlot {
            state: SlotState::Recycling {
                conn,
                ctx_id: "x".into(),
                target_id: None,
                phase: RecyclePhase::CreateCtx,
            },
            terminator: AtomicBool::new(false),
            last_used: Instant::now(),
        };
        assert!(slot.take_conn().is_some());
        assert!(matches!(slot.state, SlotState::Closing));
    }

    // ── bookkeeping / registry / pool-level invariants ────────────────

    #[tokio::test]
    async fn inflight_and_idle_len_track_across_multiple_acquire_release_cycles() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        for i in 0..3 {
            let g = pool.acquire().await.unwrap();
            assert_eq!(pool.inflight(), 1);
            g.record_target(format!("t-{i}"));
            g.release().await.unwrap();
            assert_eq!(pool.inflight(), 0);
            assert_eq!(pool.idle_len(), 1);
        }
    }

    #[tokio::test]
    async fn all_slots_registry_does_not_grow_on_slot_reuse() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let g1 = pool.acquire().await.unwrap();
        g1.record_target("t-1".into());
        g1.release().await.unwrap();
        assert_eq!(pool.all_slots.lock().unwrap().len(), 1);

        let g2 = pool.acquire().await.unwrap();
        g2.record_target("t-2".into());
        g2.release().await.unwrap();
        assert_eq!(
            pool.all_slots.lock().unwrap().len(),
            1,
            "reusing the same slot must not register a second entry"
        );
    }

    #[tokio::test]
    async fn multiple_pools_do_not_share_inflight_state() {
        let pool_a = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let pool_b = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let _guard_a = pool_a.acquire().await.unwrap();
        assert_eq!(pool_a.inflight(), 1);
        assert_eq!(pool_b.inflight(), 0);
    }

    #[tokio::test]
    async fn guard_ctx_id_and_conn_are_populated_on_acquire() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        assert!(!guard.ctx_id.is_empty());
        assert!(!guard.conn.is_closed());
    }

    #[tokio::test]
    async fn is_closed_reflects_shutdown_state() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        assert!(!pool.is_closed());
        pool.clone().shutdown(Duration::from_millis(50)).await;
        assert!(pool.is_closed());
    }

    // ── shutdown edge cases ────────────────────────────────────────────

    #[tokio::test]
    async fn shutdown_is_idempotent_when_called_twice() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        pool.clone().shutdown(Duration::from_millis(50)).await;
        pool.clone().shutdown(Duration::from_millis(50)).await;
        assert!(pool.is_closed());
    }

    #[tokio::test]
    async fn shutdown_with_nothing_acquired_completes_well_before_the_drain_deadline() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let start = Instant::now();
        pool.clone().shutdown(Duration::from_secs(2)).await;
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "an idle pool must not wait out the full drain deadline"
        );
        assert!(pool.is_closed());
    }

    #[tokio::test]
    async fn shutdown_zero_drain_still_force_closes_a_leaked_guard() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        let conn = guard.conn.clone();
        std::mem::forget(guard);
        pool.clone().shutdown(Duration::ZERO).await;
        assert_eq!(pool.inflight(), 0);
        assert_eq!(conn.close_conn_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_phase3_reconciles_inflight_for_multiple_leaked_guards() {
        let pool = BrowserContextPool::new(
            PoolCfg {
                size: 3,
                ..small_pool_cfg()
            },
            fake_factory(),
        );
        let leaked_a = pool.acquire().await.unwrap();
        let leaked_b = pool.acquire().await.unwrap();
        let conn_a = leaked_a.conn.clone();
        let conn_b = leaked_b.conn.clone();
        std::mem::forget(leaked_a);
        std::mem::forget(leaked_b);
        assert_eq!(pool.inflight(), 2);

        pool.clone().shutdown(Duration::from_millis(100)).await;
        assert_eq!(pool.inflight(), 0);
        assert_eq!(conn_a.close_conn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(conn_b.close_conn_calls.load(Ordering::SeqCst), 1);
    }

    // ── BookkeepingToken ────────────────────────────────────────────────

    #[tokio::test]
    async fn bookkeeping_token_drop_without_commit_still_decrements() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        pool.inflight.fetch_add(1, Ordering::SeqCst);
        let token = BookkeepingToken::<FakeConn>::new(Arc::downgrade(&pool));
        drop(token);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn bookkeeping_token_commit_decrement_decrements_exactly_once() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        pool.inflight.fetch_add(1, Ordering::SeqCst);
        let token = BookkeepingToken::<FakeConn>::new(Arc::downgrade(&pool));
        token.commit_decrement();
        assert_eq!(pool.inflight(), 0);
    }

    // ── Concurrency-correctness regressions ─────────────────────────────

    #[test]
    fn acquire_does_not_deadlock_on_stale_non_idle_slot() {
        // Regression: the health-check snapshot read `s.lock().unwrap().state`
        // directly as the `match` scrutinee. That MutexGuard temporary lives
        // until the end of the whole `match`, so the non-`Idle` arm called
        // `mark_slot_dead_and_drop` while the guard was still held. That
        // helper re-locks the same non-reentrant `StdMutex`. The result was a
        // hard self-deadlock.
        //
        // Driven from a plain `#[test]` on a dedicated thread rather than
        // `#[tokio::test]`: the deadlock blocks an OS *thread*, so no timeout
        // future on that same runtime can ever be polled to catch it. Only an
        // observer on a separate thread can turn the hang into an assertion.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
                let conn = Arc::new(FakeConn::default());
                // Stale enough that `acquire()` health-checks it (cfg: 60s).
                let slot = stale_idle_slot(conn, "ctx-stale", Duration::from_secs(120));
                // ...but no longer `Idle`, the shutdown-race state the `_` arm
                // exists to handle.
                slot.lock().unwrap().state = SlotState::Closing;
                pool.idle.lock().unwrap().push_back(slot);

                let acquired = pool.acquire().await.is_ok();
                let _ = tx.send(acquired);
            });
        });

        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(acquired) => assert!(
                acquired,
                "acquire should discard the dead slot and create a fresh one"
            ),
            // Distinguish the two: `Disconnected` fires immediately if anything
            // inside `block_on` panicked, and reporting that as a deadlock would
            // send the next reader hunting a lock bug that isn't there.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("acquire() self-deadlocked on a stale non-Idle slot")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the acquire worker panicked before reporting a result")
            }
        }
    }

    #[test]
    fn over_decrement_floors_at_zero() {
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        pool.inflight.store(1, Ordering::SeqCst);

        pool.dec_inflight_and_notify();
        pool.dec_inflight_and_notify();

        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn guard_drop_survives_a_poisoned_slot_mutex() {
        // Regression: `PoolGuard::drop` unwrapped the slot mutex twice, despite
        // the panic-free invariant documented on `BookkeepingToken`. `Drop` runs
        // during future cancellation; a panic there while already unwinding
        // aborts the process. `record_target` was already poison-tolerant.
        // `Drop` needs it strictly more.
        let pool = BrowserContextPool::new(small_pool_cfg(), fake_factory());
        let guard = pool.acquire().await.unwrap();
        guard.record_target("t-1".into());
        let slot = guard.slot.clone().expect("guard holds a slot");

        // Poison the slot mutex from another thread.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = slot.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(slot.is_poisoned(), "slot mutex should be poisoned");

        // Must not panic.
        drop(guard);

        assert_eq!(
            pool.inflight(),
            0,
            "Drop must still complete its bookkeeping on a poisoned slot"
        );
    }
}
