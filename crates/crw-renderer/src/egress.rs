//! Per-host egress memory: remember which hosts hard-block our direct egress.
//!
//! Without this, every URL on a blocking host re-climbs the whole renderer
//! ladder from scratch (`direct → 429 → proxy retry → JS ladder → hard block →
//! chrome_proxy`), because the block signal lives in request-local state and
//! dies at function exit. On a site that 429s us that costs 10-20s *per URL*.
//!
//! ## The latch is a TTL cache, not a state machine
//!
//! Presence in the cache == "this host recently hard-blocked our direct egress".
//! Everything else falls out of the TTL, with no explicit probe/clear code:
//!
//! - within the TTL  → [`should_proxy`] is true → try the proxy egress first;
//! - TTL expires     → entry is gone → direct is tried again. *That expiry is
//!   the half-open probe*;
//! - blocked again   → [`note_block`] re-inserts and the TTL resets (re-latch);
//! - recovered       → nothing to do; the entry already expired.
//!
//! This is deliberately not built on [`crate::preference::HostPreferences`]:
//! its `record_success` fires regardless of which tier succeeded, so a latch
//! living there would be erased by the very proxied success it produced. A TTL
//! cache needs no success hook, so nothing can erase it by mistake.
//!
//! ## The latch reorders egress; it never suppresses direct
//!
//! A latched host is *preferred* onto the proxy, but the caller must still keep
//! [`DIRECT_FALLBACK_RESERVE`] of its deadline so a failing — or, worse,
//! *hanging* — proxy can always fall back to a direct attempt. Suppressing
//! direct outright would mean a falsely-latched host whose proxy egress is worse
//! (origin blocks the proxy ranges, proxy down, geo exit trips another wall) has
//! no way to recover for the whole cooldown, which is how this could push scrape
//! success below the 89.7% red line.
//!
//! ## Writes are direct-only
//!
//! Only a block observed on a genuine direct attempt may latch a
//! host. A block seen while already egressing through a proxy says the *proxy*
//! is blocked and says nothing about direct — latching on it would let one
//! caller's broken proxy demote every other caller's healthy direct traffic onto
//! paid bandwidth.

use moka::future::Cache;
use std::sync::LazyLock;
use std::time::Duration;

use crate::preference::normalize_host;

/// Process-wide egress memory, shared by every fetch path (`/scrape`, `/crawl`,
/// `/map`), mirroring how [`crate::host_limiter`] holds its per-host state. A
/// block learned by one request is what makes the *next* one cheap, so the
/// memory has to outlive any single request or renderer instance.
static EGRESS_MEMORY: LazyLock<EgressMemory> = LazyLock::new(EgressMemory::with_defaults);

/// The shared [`EgressMemory`].
pub fn global() -> &'static EgressMemory {
    &EGRESS_MEMORY
}

/// How long a host stays latched to proxy-first egress after a hard block.
/// Also the re-probe interval: when it expires, direct is tried again.
pub const COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// Budget held back from the proxy-first attempt so a direct fallback is always
/// possible. A hanging proxy must not be able to eat the whole deadline.
pub const DIRECT_FALLBACK_RESERVE: Duration = Duration::from_secs(4);

/// Smallest budget a proxy-first attempt is worth making with.
pub const MIN_PROXY_ATTEMPT: Duration = Duration::from_secs(2);

/// The latch only takes effect when the deadline can afford BOTH a real proxy
/// attempt and a full direct rescue.
///
/// Splitting a short budget between the two is worse than not trying the proxy at
/// all: with the SaaS's 5s scrape deadline, a hanging proxy would burn half of it
/// and leave too little for direct, so a request that used to succeed over direct
/// in 3s would now fail on BOTH legs. That is a scrape-success regression, and the
/// 89.7% line is not negotiable.
///
/// So below this threshold the latch is inert and behaviour is byte-for-byte what
/// it is today. Above it the proxy gets `remaining - DIRECT_FALLBACK_RESERVE` and
/// direct still keeps a full-size rescue.
///
/// The threshold has to be picked against the REAL budgets, with slack on BOTH
/// sides, not as a round number:
///   * `/scrape` on the SaaS runs a **5s** deadline -> comfortably below, inert.
///   * `/map` and `/crawl` pages run `effective_deadline_ms` = **8s** by default
///     -> comfortably above, active. This is the path where re-climbing the ladder
///     on a blocking host cost 10-20s per URL.
///
/// Two ways to get this wrong, both of which silently turn the feature into dead
/// code while it still *looks* implemented:
///   * too high (15s was the first attempt) -> never engages on any real path;
///   * exactly equal to a budget (8s) -> `Deadline::remaining()` is always a hair
///     UNDER the budget it was built from, because time passes between construction
///     and the check. A `>= 8s` gate therefore never opens on an 8s budget, and any
///     test pinned to that boundary is decided by clock resolution.
///
/// 6s sits between the two real budgets with ~1s of slack on the scrape side and
/// ~2s on the map side, so neither is at the mercy of elapsed-time jitter.
pub const MIN_BUDGET_FOR_LATCH: Duration =
    Duration::from_secs(DIRECT_FALLBACK_RESERVE.as_secs() + MIN_PROXY_ATTEMPT.as_secs());

/// Maximum number of distinct hosts tracked.
const CAPACITY: u64 = 10_000;

/// Per-host memory of hosts that hard-block direct egress. Cheap to clone.
#[derive(Clone)]
pub struct EgressMemory {
    /// Presence == latched. The value carries no information.
    cache: Cache<String, ()>,
}

impl EgressMemory {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(CAPACITY)
                // time_to_live, NOT time_to_idle: the latch must expire a fixed
                // time after the block, so direct gets re-probed. time_to_idle
                // would be kept alive forever by the very proxied traffic the
                // latch causes, pinning the host to paid egress permanently.
                .time_to_live(cooldown)
                .build(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(COOLDOWN)
    }

    /// Latch `host` onto proxy-first egress for the cooldown.
    ///
    /// Callers MUST only invoke this for a block seen on a genuine direct
    /// attempt, and only for a strong block signal (429, `cf-mitigated`, or an
    /// antibot `classify()` verdict). A bare 401/403/503 usually means
    /// auth-required, paywall, or a transient upstream fault — none of which a
    /// residential proxy fixes, so latching on them just burns paid bandwidth.
    pub async fn note_block(&self, host: &str) {
        self.cache.insert(normalize_host(host), ()).await;
    }

    /// True while `host` is latched: try the proxy egress first.
    pub async fn should_proxy(&self, host: &str) -> bool {
        self.cache.get(&normalize_host(host)).await.is_some()
    }

    /// Drop `host` before its TTL runs out (the memory proved wrong).
    pub async fn forget(&self, host: &str) {
        self.cache.invalidate(&normalize_host(host)).await;
    }

    /// Number of currently latched hosts (approximate; for the metrics gauge).
    pub fn latched_hosts(&self) -> u64 {
        self.cache.entry_count()
    }

    #[cfg(test)]
    async fn sync(&self) {
        self.cache.run_pending_tasks().await;
    }
}

/// Per-host residential exit that served the host: normalized host -> the
/// sticky proxy session id it was served through. Same TTL and key as
/// [`EgressMemory`]. Reusing the exit is what keeps a host served: a
/// Cloudflare-fronted site scores each residential IP on its own, so a fresh
/// exit per request turns a host that worked into a coin flip, and the
/// `cf_clearance` a solved challenge earns is bound to the exit's IP.
#[derive(Clone)]
pub struct StickySessions {
    cache: Cache<String, String>,
}

impl StickySessions {
    pub fn with_defaults() -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(CAPACITY)
                // time_to_live for the same reason as `EgressMemory`: expiry
                // re-probes the normal order.
                .time_to_live(COOLDOWN)
                .build(),
        }
    }

    /// Remember that `sessid` served `host`.
    pub async fn remember(&self, host: &str, sessid: String) {
        self.cache.insert(normalize_host(host), sessid).await;
    }

    /// The session that served `host`, while it is remembered.
    pub async fn get(&self, host: &str) -> Option<String> {
        self.cache.get(&normalize_host(host)).await
    }

    /// Drop `host` (the remembered exit stopped serving it).
    pub async fn forget(&self, host: &str) {
        self.cache.invalidate(&normalize_host(host)).await;
    }
}

impl Default for EgressMemory {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_host_is_not_latched() {
        let m = EgressMemory::with_defaults();
        assert!(!m.should_proxy("example.com").await);
    }

    #[tokio::test]
    async fn note_block_latches_the_host() {
        let m = EgressMemory::with_defaults();
        m.note_block("news.ycombinator.com").await;
        assert!(m.should_proxy("news.ycombinator.com").await);
    }

    #[tokio::test]
    async fn latch_is_keyed_by_etld_plus_one() {
        let m = EgressMemory::with_defaults();
        m.note_block("a.example.com").await;
        // Subdomains of one registered domain share a latch: the block is an
        // origin-level decision, not a per-subdomain one.
        assert!(m.should_proxy("b.example.com").await);
    }

    #[tokio::test]
    async fn other_hosts_are_unaffected() {
        let m = EgressMemory::with_defaults();
        m.note_block("blocked.com").await;
        assert!(!m.should_proxy("healthy.com").await);
    }

    // moka drives its TTL off its own (quanta) clock, not tokio's, so a paused
    // tokio runtime cannot advance it. These two tests use a real, very short
    // cooldown instead.
    const TINY: Duration = Duration::from_millis(50);

    /// The regression test for the permanent-latch bug: a latched host MUST
    /// return to direct egress once the cooldown expires, otherwise a single
    /// transient 429 pins it to paid proxy bandwidth forever. TTL expiry *is*
    /// the half-open probe.
    #[tokio::test]
    async fn latch_expires_so_direct_is_reprobed() {
        let m = EgressMemory::new(TINY);
        m.note_block("news.ycombinator.com").await;
        assert!(m.should_proxy("news.ycombinator.com").await);

        tokio::time::sleep(TINY * 3).await;
        m.sync().await;

        assert!(
            !m.should_proxy("news.ycombinator.com").await,
            "latch must expire so direct egress is probed again"
        );
    }

    /// A still-blocking host re-latches on the next block, so a genuinely
    /// hostile origin does not flap back to direct on every request.
    #[tokio::test]
    async fn reblock_after_expiry_relatches() {
        let m = EgressMemory::new(TINY);
        m.note_block("hostile.com").await;

        tokio::time::sleep(TINY * 3).await;
        m.sync().await;
        assert!(!m.should_proxy("hostile.com").await);

        m.note_block("hostile.com").await;
        assert!(m.should_proxy("hostile.com").await);
    }
}
