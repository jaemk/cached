//! Parity certification: `ExpiringLruCache` vs the `LruTtlCache` oracle.
//!
//! The stated goal of the perf change was for `ExpiringLruCache` to fire `on_evict` with
//! the same key instance `LruTtlCache` does. Each test here drives the SAME scenario
//! through both stores with the same coarse-`Eq` key and asserts (a) the expected key tag
//! and (b) that the two stores agree. A disagreement on any path is a finding.
//!
//! The two stores differ only in how a value becomes stale: `ExpiringLruCache` asks the
//! value (`Expires::is_expired`), `LruTtlCache` compares a per-entry deadline against the
//! clock. Where a test needs a stale entry it makes the `ExpiringLruCache` value report
//! `is_expired() == true` and lets the `LruTtlCache` TTL lapse via a sleep.
//!
//! Gated on `time_stores` (the feature `LruTtlCache` lives behind).
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{CacheEvict, Cached, Expires, ExpiringLruCache, LruTtlCache};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// TTL short enough to keep the suite fast, long enough not to flake on a loaded machine.
const TTL: Duration = Duration::from_millis(60);
/// Slept when an entry must be past its TTL.
const LAPSE: std::time::Duration = std::time::Duration::from_millis(180);

#[derive(Debug, Clone)]
struct TagKey {
    id: u32,
    tag: &'static str,
}

impl TagKey {
    fn new(id: u32, tag: &'static str) -> Self {
        Self { id, tag }
    }
}

impl PartialEq for TagKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TagKey {}
impl Hash for TagKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug, Clone)]
struct Val {
    expired: bool,
}

impl Val {
    fn live() -> Self {
        Self { expired: false }
    }
    fn stale() -> Self {
        Self { expired: true }
    }
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

type Log = Arc<Mutex<Vec<&'static str>>>;

fn expiring(max_size: usize) -> (ExpiringLruCache<TagKey, Val>, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let cache = ExpiringLruCache::builder()
        .max_size(max_size)
        .on_evict(move |k: &TagKey, _v: &Val| sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build ExpiringLruCache");
    (cache, log)
}

fn timed(max_size: usize) -> (LruTtlCache<TagKey, u32>, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let cache = LruTtlCache::builder()
        .max_size(max_size)
        .ttl(TTL)
        .on_evict(move |k: &TagKey, _v: &u32| sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build LruTtlCache");
    (cache, log)
}

fn seen(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

/// Assert both stores saw `expected`, and say so in terms of parity when they do not.
fn assert_parity(expiring_log: &Log, timed_log: &Log, expected: &[&'static str]) {
    let e = seen(expiring_log);
    let t = seen(timed_log);
    assert_eq!(
        e, t,
        "ExpiringLruCache and LruTtlCache must agree on the on_evict keys"
    );
    assert_eq!(e, expected, "both stores must report the stored key(s)");
}

#[test]
fn parity_cache_set_over_expired() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    assert!(e.cache_set(TagKey::new(1, "caller"), Val::live()).is_none());

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    assert!(t.cache_set(TagKey::new(1, "caller"), 2).is_none());

    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_cache_get_lazy_sweep() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    assert!(e.cache_get(&TagKey::new(1, "lookup")).is_none());

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    assert!(t.cache_get(&TagKey::new(1, "lookup")).is_none());

    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_cache_get_mut_lazy_sweep() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    assert!(e.cache_get_mut(&TagKey::new(1, "lookup")).is_none());

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    assert!(t.cache_get_mut(&TagKey::new(1, "lookup")).is_none());

    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_cache_remove_entry_returns_the_stored_key() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    let (ek, _) = e
        .cache_remove_entry(&TagKey::new(1, "lookup"))
        .expect("present");

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    let (tk, _) = t
        .cache_remove_entry(&TagKey::new(1, "lookup"))
        .expect("present");

    assert_eq!(
        ek.tag, tk.tag,
        "both stores must return the same key instance from cache_remove_entry"
    );
    assert_eq!(ek.tag, "stored");
    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_capacity_eviction_reports_the_rebound_key() {
    // An overwrite rebinds the stored key AND promotes the entry to MRU; a later capacity
    // eviction of that same entry must report the rebound key in both stores. The read of
    // key 2 puts key 1 back at the LRU position so it is the victim again.
    let (mut e, elog) = expiring(2);
    e.cache_set(TagKey::new(1, "first"), Val::live());
    e.cache_set(TagKey::new(2, "k2"), Val::live());
    e.cache_set(TagKey::new(1, "second"), Val::live());
    assert!(e.cache_get(&TagKey::new(2, "?")).is_some());
    e.cache_set(TagKey::new(3, "k3"), Val::live());

    let (mut t, tlog) = timed(2);
    t.cache_set(TagKey::new(1, "first"), 1);
    t.cache_set(TagKey::new(2, "k2"), 2);
    t.cache_set(TagKey::new(1, "second"), 11);
    assert!(t.cache_get(&TagKey::new(2, "?")).is_some());
    t.cache_set(TagKey::new(3, "k3"), 3);

    assert_parity(&elog, &tlog, &["second"]);
}

#[test]
fn parity_evict_sweep() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "s1"), Val::stale());
    e.cache_set(TagKey::new(2, "s2"), Val::stale());
    assert_eq!(CacheEvict::evict(&mut e), 2);

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "s1"), 1);
    t.cache_set(TagKey::new(2, "s2"), 2);
    std::thread::sleep(LAPSE);
    assert_eq!(CacheEvict::evict(&mut t), 2);

    // MRU -> LRU sweep order.
    assert_parity(&elog, &tlog, &["s2", "s1"]);
}

#[test]
fn parity_retain_predicate_and_expiry_limbs() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "by_pred"), Val::live());
    e.cache_set(TagKey::new(2, "keep"), Val::live());
    e.retain(|k, _| k.id != 1);

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "by_pred"), 1);
    t.cache_set(TagKey::new(2, "keep"), 2);
    t.retain(|k, _| k.id != 1);

    assert_parity(&elog, &tlog, &["by_pred"]);

    // Now the expiry limb: an expired entry is removed WITHOUT consulting the predicate.
    let (mut e2, elog2) = expiring(8);
    e2.cache_set(TagKey::new(1, "stale"), Val::stale());
    e2.retain(|_, _| true);

    let (mut t2, tlog2) = timed(8);
    t2.cache_set(TagKey::new(1, "stale"), 1);
    std::thread::sleep(LAPSE);
    t2.retain(|_, _| true);

    assert_parity(&elog2, &tlog2, &["stale"]);
}

#[test]
fn parity_cache_clear_with_on_evict_mru_to_lru() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "first"), Val::live());
    e.cache_set(TagKey::new(2, "k2"), Val::live());
    e.cache_set(TagKey::new(1, "second"), Val::live());
    e.cache_clear_with_on_evict();

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "first"), 1);
    t.cache_set(TagKey::new(2, "k2"), 2);
    t.cache_set(TagKey::new(1, "second"), 11);
    t.cache_clear_with_on_evict();

    // The overwrite of key 1 promoted it, so the MRU -> LRU drain reports it first.
    assert_parity(&elog, &tlog, &["second", "k2"]);
}

#[test]
fn parity_cache_clear_is_silent() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "k1"), Val::live());
    e.cache_clear();

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "k1"), 1);
    t.cache_clear();

    assert_parity(&elog, &tlog, &[]);
}

#[test]
fn parity_set_max_size_shrink() {
    let (mut e, elog) = expiring(4);
    e.cache_set(TagKey::new(1, "k1"), Val::live());
    e.cache_set(TagKey::new(2, "k2"), Val::live());
    e.cache_set(TagKey::new(3, "k3"), Val::live());
    let _ = e.set_max_size(1);

    let (mut t, tlog) = timed(4);
    t.cache_set(TagKey::new(1, "k1"), 1);
    t.cache_set(TagKey::new(2, "k2"), 2);
    t.cache_set(TagKey::new(3, "k3"), 3);
    let _ = t.set_max_size(1);

    assert_parity(&elog, &tlog, &["k1", "k2"]);
}

#[test]
fn parity_get_or_set_with_over_expired() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    let _ = e.cache_get_or_set_with(TagKey::new(1, "caller"), Val::live);

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    let _ = t.cache_get_or_set_with(TagKey::new(1, "caller"), || 2);

    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_try_get_or_set_with_ok_over_expired() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    let r: Result<&Val, ()> =
        e.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Ok(Val::live()));
    assert!(r.is_ok());

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    let r: Result<&u32, ()> = t.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Ok(2));
    assert!(r.is_ok());

    assert_parity(&elog, &tlog, &["stored"]);
}

#[test]
fn parity_try_get_or_set_with_err_over_expired_evicts_nothing() {
    let (mut e, elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    let r: Result<&Val, &str> = e.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Err("x"));
    assert!(r.is_err());

    let (mut t, tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    let r: Result<&u32, &str> = t.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Err("x"));
    assert!(r.is_err());

    assert_parity(&elog, &tlog, &[]);
    assert_eq!(e.cache_size(), 1, "the stale entry survives an Err factory");
    assert_eq!(t.cache_size(), 1, "the stale entry survives an Err factory");
}

/// On a fallible get-or-set whose factory returns `Err` over an expired entry, both stores
/// count the miss: the lookup found no live entry, and whether the initializer then fails
/// does not change that. Each increments inside the setter, which the inner store invokes
/// only when the lookup missed.
///
/// `LruTtlCache` previously lost this miss (it incremented after the store call, which the
/// `?` early return skipped) and was the sole outlier in the family; `UnboundCache`,
/// `LruCache`, `TtlCache` and `ExpiringLruCache` all already counted it.
///
/// Neither store fires `on_evict` or counts an eviction here: on `Err` the expired entry is
/// still physically stored, so firing now would double-fire when a later call really
/// displaces it. That is asserted in the lru_ttl certification suite.
#[test]
fn try_get_or_set_with_err_counts_the_miss_on_both_stores() {
    let (mut e, _elog) = expiring(8);
    e.cache_set(TagKey::new(1, "stored"), Val::stale());
    let r: Result<&Val, &str> = e.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Err("x"));
    assert!(r.is_err());

    let (mut t, _tlog) = timed(8);
    t.cache_set(TagKey::new(1, "stored"), 1);
    std::thread::sleep(LAPSE);
    let r: Result<&u32, &str> = t.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Err("x"));
    assert!(r.is_err());

    assert_eq!(
        e.cache_misses(),
        Some(1),
        "ExpiringLruCache counts the miss before running the factory (EXP-2)"
    );
    assert_eq!(
        t.cache_misses(),
        Some(1),
        "LruTtlCache counts the miss inside the setter, so an Err factory cannot drop it"
    );
}

#[cfg(feature = "async_core")]
mod async_parity {
    use super::*;
    use cached::CachedGetOrSetAsync;

    #[tokio::test]
    async fn parity_async_get_or_set_with_over_expired() {
        let (mut e, elog) = expiring(8);
        e.cache_set(TagKey::new(1, "stored"), Val::stale());
        let _ = e
            .async_cache_get_or_set_with(TagKey::new(1, "caller"), || async { Val::live() })
            .await;

        let (mut t, tlog) = timed(8);
        t.cache_set(TagKey::new(1, "stored"), 1);
        tokio::time::sleep(LAPSE).await;
        let _ = t
            .async_cache_get_or_set_with(TagKey::new(1, "caller"), || async { 2u32 })
            .await;

        assert_parity(&elog, &tlog, &["stored"]);
    }

    #[tokio::test]
    async fn parity_async_try_get_or_set_with_ok_over_expired() {
        let (mut e, elog) = expiring(8);
        e.cache_set(TagKey::new(1, "stored"), Val::stale());
        let r: Result<&Val, ()> = e
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async { Ok(Val::live()) })
            .await;
        assert!(r.is_ok());

        let (mut t, tlog) = timed(8);
        t.cache_set(TagKey::new(1, "stored"), 1);
        tokio::time::sleep(LAPSE).await;
        let r: Result<&u32, ()> = t
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async { Ok(2u32) })
            .await;
        assert!(r.is_ok());

        assert_parity(&elog, &tlog, &["stored"]);
    }

    #[tokio::test]
    async fn parity_async_try_get_or_set_with_err_evicts_nothing() {
        let (mut e, elog) = expiring(8);
        e.cache_set(TagKey::new(1, "stored"), Val::stale());
        let r: Result<&Val, &str> = e
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async { Err("x") })
            .await;
        assert!(r.is_err());

        let (mut t, tlog) = timed(8);
        t.cache_set(TagKey::new(1, "stored"), 1);
        tokio::time::sleep(LAPSE).await;
        let r: Result<&u32, &str> = t
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async { Err("x") })
            .await;
        assert!(r.is_err());

        assert_parity(&elog, &tlog, &[]);
    }
}
