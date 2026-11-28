//! Per-`(host, proxy)` Cloudflare clearance-cookie cache.
//!
//! `cf_clearance` is bound by Cloudflare to the `(IP, User-Agent, TLS/JA3)` that
//! solved the challenge. crw's Chrome renderer already supplies a real Chrome
//! UA + JA3, and the sticky-per-host proxy keeps the egress IP stable — so the
//! only thing missing to skip the interstitial on a *repeat* fetch is the cookie
//! itself. We capture it after a solve and re-inject it (CDP `Network.setCookie`)
//! before the next navigation to the same `(host, proxy)`, turning N challenge
//! solves into 1.
//!
//! This module is deliberately renderer-agnostic and side-effect-free: it owns
//! the cache + the single-flight locks, nothing else. The CDP layer drives the
//! capture/inject I/O against it.
//!
//! The single flight only applies to a key that has earned a `cf_clearance`
//! before (see [`ClearanceCache::solve_turn`]). A host that never challenges
//! never yields a cookie, so locking it turned every concurrent render of that
//! host into a queue that ran one page at a time.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

/// A captured clearance cookie. Only the fields CDP `Network.setCookie` needs to
/// faithfully replay it are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearanceCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

/// Cookie names worth caching — the Cloudflare clearance + bot-management pair.
/// Anything else (analytics, session) is the site's own and must not be replayed
/// across requests (could pin us to a stale/foreign session).
pub fn is_clearance_cookie(name: &str) -> bool {
    matches!(name, "cf_clearance" | "__cf_bm")
}

/// Refresh well before Cloudflare's ~30 min `cf_clearance` lifetime so an
/// in-flight reuse never races the expiry.
const DEFAULT_TTL: Duration = Duration::from_secs(25 * 60);

/// How long a key stays known to challenge after its last `cf_clearance`
/// capture. It outlives the cookie on purpose: the cold fetch after an expiry
/// is the one the single flight exists for.
const CHALLENGED_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// What a fetch does about the clearance cookie before it navigates.
pub enum SolveTurn {
    /// A fresh cached cookie: inject it and go, no lock.
    Cached(Vec<ClearanceCookie>),
    /// This fetch solves the key's challenge. Hold the guard until the cookie
    /// is captured so the fetches waiting on the key can reuse it.
    Solver(tokio::sync::OwnedMutexGuard<()>),
    /// Go ahead alone: no cookie to inject and no lock held.
    Alone,
}

struct Entry {
    cookies: Vec<ClearanceCookie>,
    captured_at: Instant,
}

/// Process-wide clearance cache. Two small mutexes: one for entries, one for the
/// per-key single-flight locks. Held only for map lookups, never across `.await`.
pub struct ClearanceCache {
    entries: Mutex<HashMap<String, Entry>>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// Keys that captured a `cf_clearance`, with the capture time.
    challenged: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl Default for ClearanceCache {
    fn default() -> Self {
        Self::with_ttl(DEFAULT_TTL)
    }
}

impl ClearanceCache {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            challenged: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Cache key. Pins every component Cloudflare binds `cf_clearance` to:
    /// `host` (the request host), `proxy_id` (the egress that solved it — a
    /// different IP is rejected), and `ua` (the User-Agent it was solved under —
    /// a caller-supplied override must not replay another UA's cookie).
    pub fn key(host: &str, proxy_id: Option<&str>, ua: &str) -> String {
        format!(
            "{}|{}|{}",
            host.to_ascii_lowercase(),
            proxy_id.unwrap_or("-"),
            ua
        )
    }

    /// Fresh cookies for `key`, or `None` if absent/expired. Expired entries are
    /// evicted on read.
    pub fn get(&self, key: &str) -> Option<Vec<ClearanceCookie>> {
        self.get_at(key, Instant::now())
    }

    fn get_at(&self, key: &str, now: Instant) -> Option<Vec<ClearanceCookie>> {
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            Some(e) if now.saturating_duration_since(e.captured_at) < self.ttl => {
                Some(e.cookies.clone())
            }
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Store freshly-captured clearance cookies. A capture with no clearance
    /// cookie is a no-op (don't cache an empty/non-clearance result).
    pub fn put(&self, key: &str, cookies: Vec<ClearanceCookie>) {
        if cookies.is_empty() {
            return;
        }
        if cookies.iter().any(|c| c.name == "cf_clearance") {
            self.challenged
                .lock()
                .unwrap()
                .insert(key.to_string(), Instant::now());
        }
        self.entries.lock().unwrap().insert(
            key.to_string(),
            Entry {
                cookies,
                captured_at: Instant::now(),
            },
        );
    }

    /// Drop an entry — called when a reuse still came back challenged (the cookie
    /// died early, or the egress IP changed under us).
    pub fn invalidate(&self, key: &str) {
        self.entries.lock().unwrap().remove(key);
    }

    /// The single-flight lock for `key`. Its holder solves the challenge while
    /// concurrent fetches to the key wait for its cookie instead of each meeting
    /// the challenge; once the cookie is cached the waiters re-check, find it,
    /// and proceed in parallel. The chrome tier takes it through
    /// [`Self::solve_turn`], which leaves a key that never challenged unlocked.
    pub fn solve_lock(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks.entry(key.to_string()).or_default().clone()
    }

    /// Whether `key` captured a `cf_clearance` within [`CHALLENGED_TTL`].
    fn is_challenged_at(&self, key: &str, now: Instant) -> bool {
        let mut challenged = self.challenged.lock().unwrap();
        match challenged.get(key) {
            Some(at) if now.saturating_duration_since(*at) < CHALLENGED_TTL => true,
            Some(_) => {
                challenged.remove(key);
                false
            }
            None => false,
        }
    }

    /// Decide how a fetch to `key` treats the clearance cookie.
    ///
    /// A cached cookie is injected. A key that never earned a `cf_clearance`
    /// goes alone: there is no challenge to wait for, and the first fetch's
    /// render would only produce a page, never a cookie the others could use.
    /// A key known to challenge takes the single flight, but a fetch waits for
    /// another fetch's solve at most `room`, then goes alone rather than
    /// spending its own deadline in the queue. A lock that is free is taken
    /// even when `room` is zero, so one of a burst still solves for the rest.
    pub async fn solve_turn(&self, key: &str, room: Duration) -> SolveTurn {
        if let Some(cookies) = self.get(key) {
            return SolveTurn::Cached(cookies);
        }
        if !self.is_challenged_at(key, Instant::now()) {
            return SolveTurn::Alone;
        }
        let Ok(guard) = tokio::time::timeout(room, self.solve_lock(key).lock_owned()).await else {
            return SolveTurn::Alone;
        };
        match self.get(key) {
            Some(cookies) => SolveTurn::Cached(cookies),
            None => SolveTurn::Solver(guard),
        }
    }
}

/// The process-wide cache instance.
pub fn clearance_cache() -> &'static ClearanceCache {
    static CACHE: LazyLock<ClearanceCache> = LazyLock::new(ClearanceCache::default);
    &CACHE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(name: &str) -> ClearanceCookie {
        ClearanceCookie {
            name: name.into(),
            value: "v".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: true,
        }
    }

    #[test]
    fn clearance_cookie_filter() {
        assert!(is_clearance_cookie("cf_clearance"));
        assert!(is_clearance_cookie("__cf_bm"));
        assert!(!is_clearance_cookie("_ga"));
        assert!(!is_clearance_cookie("session"));
    }

    #[test]
    fn key_lowercases_host_and_pins_proxy_and_ua() {
        assert_eq!(
            ClearanceCache::key("Example.COM", None, "UA/1"),
            "example.com|-|UA/1"
        );
        assert_eq!(
            ClearanceCache::key("example.com", Some("px1"), "UA/1"),
            "example.com|px1|UA/1"
        );
        // Different egress → different key → no cross-IP reuse.
        assert_ne!(
            ClearanceCache::key("example.com", Some("px1"), "UA/1"),
            ClearanceCache::key("example.com", Some("px2"), "UA/1")
        );
        // Different UA → different key → no cross-UA reuse (CF rejects those).
        assert_ne!(
            ClearanceCache::key("example.com", Some("px1"), "UA/1"),
            ClearanceCache::key("example.com", Some("px1"), "UA/2")
        );
    }

    #[test]
    fn put_get_roundtrip_and_empty_is_noop() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("example.com", None, "UA/1");
        assert!(c.get(&k).is_none());
        c.put(&k, vec![]); // empty → not stored
        assert!(c.get(&k).is_none());
        c.put(&k, vec![ck("cf_clearance")]);
        assert_eq!(c.get(&k).unwrap(), vec![ck("cf_clearance")]);
    }

    #[test]
    fn invalidate_drops_entry() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("example.com", Some("px1"), "UA/1");
        c.put(&k, vec![ck("cf_clearance")]);
        assert!(c.get(&k).is_some());
        c.invalidate(&k);
        assert!(c.get(&k).is_none());
    }

    #[test]
    fn expired_entry_is_evicted_on_read() {
        let c = ClearanceCache::with_ttl(Duration::from_secs(60));
        let k = ClearanceCache::key("example.com", None, "UA/1");
        c.put(&k, vec![ck("cf_clearance")]);
        // Read 61s in the future → expired.
        let future = Instant::now() + Duration::from_secs(61);
        assert!(c.get_at(&k, future).is_none());
        // And it was evicted, not just hidden.
        assert!(c.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn solve_lock_is_stable_per_key() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("example.com", None, "UA/1");
        let a = c.solve_lock(&k);
        let b = c.solve_lock(&k);
        assert!(Arc::ptr_eq(&a, &b), "same key must share one lock");
        let other = c.solve_lock(&ClearanceCache::key("other.com", None, "UA/1"));
        assert!(
            !Arc::ptr_eq(&a, &other),
            "different keys get different locks"
        );
    }

    #[tokio::test]
    async fn solve_lock_serializes_first_solve() {
        // Two tasks race on a cold key; the lock must serialize them so only the
        // first "solves" and the second finds the cached cookie.
        let c = Arc::new(ClearanceCache::default());
        let k = ClearanceCache::key("example.com", None, "UA/1");
        let solves = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let run = |c: Arc<ClearanceCache>,
                   k: String,
                   solves: Arc<std::sync::atomic::AtomicUsize>| async move {
            if c.get(&k).is_none() {
                let lock = c.solve_lock(&k);
                let _g = lock.lock_owned().await;
                if c.get(&k).is_none() {
                    solves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    c.put(&k, vec![ck("cf_clearance")]);
                }
            }
        };

        let (a, b) = tokio::join!(
            run(c.clone(), k.clone(), solves.clone()),
            run(c.clone(), k.clone(), solves.clone()),
        );
        let _ = (a, b);
        assert_eq!(solves.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn challenged(c: &ClearanceCache, k: &str) -> bool {
        c.is_challenged_at(k, Instant::now())
    }

    #[test]
    fn only_a_cf_clearance_capture_marks_the_key_challenged() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("example.com", None, "UA/1");
        c.put(&k, vec![ck("__cf_bm")]);
        assert!(!challenged(&c, &k), "__cf_bm rides on every CF response");
        c.put(&k, vec![ck("__cf_bm"), ck("cf_clearance")]);
        assert!(challenged(&c, &k));
        // The mark outlives the cookie: the cold fetch after expiry still locks.
        c.invalidate(&k);
        assert!(challenged(&c, &k));
        let later = Instant::now() + CHALLENGED_TTL + Duration::from_secs(1);
        assert!(!c.is_challenged_at(&k, later));
    }

    #[tokio::test]
    async fn a_key_that_never_challenged_is_never_locked() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("plain.example", None, "UA/1");
        let a = c.solve_turn(&k, Duration::from_secs(30)).await;
        // The first fetch's render captured nothing, so its guard (had it taken
        // one) would still be held here. The second fetch must not queue.
        let b = tokio::time::timeout(
            Duration::from_millis(50),
            c.solve_turn(&k, Duration::from_secs(30)),
        )
        .await
        .expect("second fetch to a plain host waited on a lock");
        assert!(matches!(a, SolveTurn::Alone));
        assert!(matches!(b, SolveTurn::Alone));
    }

    #[tokio::test]
    async fn a_cached_cookie_is_injected_without_the_lock() {
        let c = ClearanceCache::default();
        let k = ClearanceCache::key("cf.example", None, "UA/1");
        c.put(&k, vec![ck("cf_clearance")]);
        let _held = c.solve_lock(&k).lock_owned().await;
        let turn =
            tokio::time::timeout(Duration::from_millis(50), c.solve_turn(&k, Duration::ZERO))
                .await
                .expect("a cached cookie must not wait on the lock");
        assert!(matches!(turn, SolveTurn::Cached(v) if v == vec![ck("cf_clearance")]));
    }

    /// A challenged key whose cookie expired: the first fetch solves.
    fn cold_challenged(host: &str) -> (Arc<ClearanceCache>, String) {
        let c = Arc::new(ClearanceCache::default());
        let k = ClearanceCache::key(host, None, "UA/1");
        c.put(&k, vec![ck("cf_clearance")]);
        c.invalidate(&k);
        (c, k)
    }

    #[tokio::test]
    async fn a_free_lock_is_taken_even_with_no_room() {
        let (c, k) = cold_challenged("cf.example");
        assert!(matches!(
            c.solve_turn(&k, Duration::ZERO).await,
            SolveTurn::Solver(_)
        ));
    }

    #[tokio::test]
    async fn a_waiter_with_no_room_goes_alone_at_once() {
        let (c, k) = cold_challenged("cf.example");
        let SolveTurn::Solver(_guard) = c.solve_turn(&k, Duration::ZERO).await else {
            panic!("first fetch must solve");
        };
        let t0 = Instant::now();
        assert!(matches!(
            c.solve_turn(&k, Duration::ZERO).await,
            SolveTurn::Alone
        ));
        assert!(t0.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn a_waiter_gives_up_after_its_room() {
        let (c, k) = cold_challenged("cf.example");
        let SolveTurn::Solver(_guard) = c.solve_turn(&k, Duration::ZERO).await else {
            panic!("first fetch must solve");
        };
        let t0 = Instant::now();
        let turn = c.solve_turn(&k, Duration::from_millis(100)).await;
        assert!(matches!(turn, SolveTurn::Alone));
        let waited = t0.elapsed();
        assert!(waited >= Duration::from_millis(100), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
    }

    #[tokio::test]
    async fn a_waiter_reuses_the_solvers_cookie() {
        let (c, k) = cold_challenged("cf.example");
        let SolveTurn::Solver(guard) = c.solve_turn(&k, Duration::ZERO).await else {
            panic!("first fetch must solve");
        };
        let waiter = {
            let (c, k) = (c.clone(), k.clone());
            tokio::spawn(async move { c.solve_turn(&k, Duration::from_secs(5)).await })
        };
        tokio::task::yield_now().await;
        c.put(&k, vec![ck("cf_clearance")]);
        drop(guard);
        assert!(matches!(waiter.await.unwrap(), SolveTurn::Cached(_)));
    }
}
