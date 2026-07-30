//! Independent certification of the `ExpiringLruCache` `on_evict` key contract.
//!
//! The perf change under certification made `ExpiringLruCache::cache_set` fire `on_evict`
//! with the **stored** key (the key instance actually held by the entry) rather than the
//! caller's key, and dropped the unconditional caller-key clone. `tests/
//! v3_expiring_lru_on_evict_key.rs` covers exactly one path: `cache_set` over an expired
//! entry.
//!
//! This file certifies the contract on **every** path that can fire `on_evict`:
//!
//! | path | covered by |
//! |---|---|
//! | `cache_set` over an expired entry | `cache_set_over_expired_*` |
//! | `cache_set` capacity eviction (`LruCache::check_capacity`) | `capacity_eviction_*` |
//! | `cache_get` / `cache_get_mut` lazy expiry sweep | `cache_get*_lazy_sweep_*` |
//! | `cache_remove` / `cache_remove_entry` | `cache_remove*_*` |
//! | `evict()` sweep | `evict_sweep_*` |
//! | `retain()` (predicate and expiry limbs) | `retain_*` |
//! | `cache_clear_with_on_evict` | `cache_clear_with_on_evict_*` |
//! | `set_max_size` shrink | `set_max_size_shrink_*` |
//! | `cache_get_or_set_with` (sync, infallible) | `get_or_set_with_*` |
//! | `cache_try_get_or_set_with` (sync, fallible, Ok and Err) | `try_get_or_set_with_*` |
//! | `async_cache_get_or_set_with` (async, infallible) | `async_get_or_set_with_*` |
//! | `async_cache_try_get_or_set_with` (async, fallible, Ok and Err) | `async_try_get_or_set_with_*` |
//!
//! The observation instrument is a *coarse-`Eq`* key: `TagKey`'s `Hash`/`Eq` cover only
//! `id`, so `TagKey { id: 1, tag: "stored" }` and `TagKey { id: 1, tag: "caller" }` are
//! interchangeable for lookup but distinguishable inside the callback. Every assertion
//! names the exact instance the callback must observe.
//!
//! Parity against the `LruTtlCache` oracle lives in
//! `tests/v3_cert_expiring_lru_lru_ttl_parity.rs`.

use cached::{CacheEvict, Cached, Expires, ExpiringLruCache};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

// --- instruments ---------------------------------------------------------------------

/// A key whose `Hash`/`Eq` cover only `id`. `tag` rides along uncompared, so two `TagKey`s
/// with the same `id` are equal-but-distinguishable instances.
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

/// A value that decides its own staleness and carries a label so the callback can also be
/// checked for handing back the *displaced* value rather than the incoming one.
#[derive(Debug, Clone)]
struct Val {
    expired: bool,
    label: &'static str,
}

impl Val {
    fn live(label: &'static str) -> Self {
        Self {
            expired: false,
            label,
        }
    }
    fn stale(label: &'static str) -> Self {
        Self {
            expired: true,
            label,
        }
    }
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

type Log = Arc<Mutex<Vec<(&'static str, &'static str)>>>;

/// Build a cache with a recording `on_evict`; returns the cache and the log of
/// `(key tag, value label)` pairs in firing order.
fn cache_with_log(max_size: usize) -> (ExpiringLruCache<TagKey, Val>, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let cache = ExpiringLruCache::builder()
        .max_size(max_size)
        .on_evict(move |k: &TagKey, v: &Val| sink.lock().unwrap().push((k.tag, v.label)))
        .build()
        .expect("build ExpiringLruCache");
    (cache, log)
}

fn seen(log: &Log) -> Vec<(&'static str, &'static str)> {
    log.lock().unwrap().clone()
}

/// Sanity check on the instrument itself: without this, every assertion below is vacuous.
#[test]
fn tag_key_is_equal_but_distinguishable() {
    let a = TagKey::new(1, "stored");
    let b = TagKey::new(1, "caller");
    assert_eq!(a, b, "same id must compare equal");
    assert_ne!(a.tag, b.tag, "tags must differ to tell the instances apart");
    let hash = |k: &TagKey| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        h.finish()
    };
    assert_eq!(hash(&a), hash(&b), "same id must hash equal");
    assert_ne!(
        TagKey::new(1, "x"),
        TagKey::new(2, "x"),
        "different ids must not compare equal"
    );
}

// --- cache_set -----------------------------------------------------------------------

#[test]
fn cache_set_over_expired_reports_stored_key_and_displaced_value() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    assert!(
        c.cache_set(TagKey::new(1, "caller"), Val::live("new"))
            .is_none(),
        "an expired displaced value is filtered from the return"
    );
    assert_eq!(
        seen(&log),
        vec![("stored", "old")],
        "on_evict must see the STORED key and the DISPLACED value"
    );
    assert_eq!(c.cache_evictions(), Some(1));
}

#[test]
fn cache_set_over_live_does_not_fire_but_rebinds_the_stored_key() {
    // The LRU primitive `order.set` replaces the whole `(K, V)` slot, so overwriting an
    // existing key silently rebinds the stored key to the caller's instance. This is
    // NOT `HashMap::insert` semantics (which keeps the original key) -- pinning it here
    // because every later on_evict on that entry reports the rebound key.
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "first"), Val::live("v1"));
    let displaced = c.cache_set(TagKey::new(1, "second"), Val::live("v2"));
    assert_eq!(
        displaced.map(|v| v.label),
        Some("v1"),
        "a live displaced value is returned"
    );
    assert!(
        seen(&log).is_empty(),
        "overwriting a live entry must not fire on_evict"
    );

    // Observe the stored key through cache_remove_entry, using a third distinct instance.
    let (stored_key, _) = c
        .cache_remove_entry(&TagKey::new(1, "probe"))
        .expect("entry present");
    assert_eq!(
        stored_key.tag, "second",
        "cache_set over a live entry rebinds the stored key to the caller's instance"
    );
}

// --- capacity eviction ---------------------------------------------------------------

#[test]
fn capacity_eviction_reports_the_victims_stored_key() {
    let (mut c, log) = cache_with_log(2);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::live("v2"));
    c.cache_set(TagKey::new(3, "k3"), Val::live("v3")); // evicts LRU = id 1
    assert_eq!(
        seen(&log),
        vec![("k1", "v1")],
        "capacity eviction reports the victim's own stored key/value"
    );
    assert_eq!(c.cache_size(), 2);
}

#[test]
fn capacity_eviction_reports_the_rebound_key_after_an_overwrite() {
    // Combines the two facts above: an overwrite rebinds the stored key (and promotes the
    // entry to MRU), so a later capacity eviction of that same entry must report the
    // OVERWRITING key, not the originally inserted one.
    let (mut c, log) = cache_with_log(2);
    c.cache_set(TagKey::new(1, "first"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::live("v2"));
    // Overwrite of id 1: rebinds the key AND promotes it to MRU.
    c.cache_set(TagKey::new(1, "second"), Val::live("v1b"));
    assert!(seen(&log).is_empty(), "no eviction yet");
    // Read id 2 to push id 1 back to the LRU position, so it is the next victim.
    assert!(c.cache_get(&TagKey::new(2, "ignored")).is_some());
    c.cache_set(TagKey::new(3, "k3"), Val::live("v3"));
    assert_eq!(
        seen(&log),
        vec![("second", "v1b")],
        "the victim's stored key is the one bound by the most recent cache_set"
    );
}

#[test]
fn capacity_eviction_of_an_expired_victim_fires_exactly_once() {
    // Both the outer ExpiringLruCache and its inner LruCache hold the same on_evict Arc.
    // A new-key insert that evicts an expired LRU victim must still fire exactly once
    // (the inner check_capacity path), never twice.
    let (mut c, log) = cache_with_log(1);
    c.cache_set(TagKey::new(1, "victim"), Val::stale("old"));
    c.cache_set(TagKey::new(2, "fresh"), Val::live("new"));
    assert_eq!(
        seen(&log),
        vec![("victim", "old")],
        "an expired capacity victim fires on_evict exactly once"
    );
    assert_eq!(c.cache_evictions(), Some(1));
}

// --- lazy expiry sweep on read -------------------------------------------------------

#[test]
fn cache_get_lazy_sweep_reports_stored_key_not_lookup_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    assert!(
        c.cache_get(&TagKey::new(1, "lookup")).is_none(),
        "an expired entry is not returned"
    );
    assert_eq!(
        seen(&log),
        vec![("stored", "old")],
        "the lazy sweep must report the STORED key, not the lookup key"
    );
    assert_eq!(c.cache_size(), 0, "the entry is physically removed");
}

#[test]
fn cache_get_mut_lazy_sweep_reports_stored_key_not_lookup_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    assert!(c.cache_get_mut(&TagKey::new(1, "lookup")).is_none());
    assert_eq!(seen(&log), vec![("stored", "old")]);
    assert_eq!(c.cache_size(), 0);
}

// --- explicit removal ----------------------------------------------------------------

#[test]
fn cache_remove_reports_stored_key_for_a_live_entry() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::live("v1"));
    let removed = c.cache_remove(&TagKey::new(1, "lookup"));
    assert_eq!(removed.map(|v| v.label), Some("v1"));
    assert_eq!(seen(&log), vec![("stored", "v1")]);
}

#[test]
fn cache_remove_reports_stored_key_for_an_expired_entry_and_returns_none() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    assert!(
        c.cache_remove(&TagKey::new(1, "lookup")).is_none(),
        "cache_remove filters an expired value from its return"
    );
    assert_eq!(seen(&log), vec![("stored", "old")]);
    assert_eq!(c.cache_size(), 0, "the entry is removed regardless");
}

#[test]
fn cache_remove_entry_returns_and_reports_the_stored_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    let (k, v) = c
        .cache_remove_entry(&TagKey::new(1, "lookup"))
        .expect("entry present");
    assert_eq!(
        k.tag, "stored",
        "cache_remove_entry must hand back the STORED key"
    );
    assert_eq!(v.label, "old", "and the stored value regardless of expiry");
    assert_eq!(seen(&log), vec![("stored", "old")]);
}

#[test]
fn cache_remove_of_an_absent_key_does_not_fire() {
    let (mut c, log) = cache_with_log(8);
    assert!(c.cache_remove(&TagKey::new(9, "ghost")).is_none());
    assert!(c.cache_remove_entry(&TagKey::new(9, "ghost")).is_none());
    assert!(seen(&log).is_empty());
    assert_eq!(c.cache_evictions(), Some(0));
}

// --- evict() sweep -------------------------------------------------------------------

#[test]
fn evict_sweep_reports_stored_keys_only_for_expired_entries() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "s1"), Val::stale("old1"));
    c.cache_set(TagKey::new(2, "live"), Val::live("keep"));
    c.cache_set(TagKey::new(3, "s3"), Val::stale("old3"));
    let removed = CacheEvict::evict(&mut c);
    assert_eq!(removed, 2);
    // Sweep order is MRU -> LRU: 3 was inserted last.
    assert_eq!(
        seen(&log),
        vec![("s3", "old3"), ("s1", "old1")],
        "evict reports each expired entry's own stored key"
    );
    assert_eq!(c.cache_size(), 1);
}

#[test]
fn evict_sweep_reports_the_rebound_key_after_an_overwrite() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "first"), Val::live("v1"));
    // Overwrite the live entry with a stale value under a distinct key instance.
    c.cache_set(TagKey::new(1, "second"), Val::stale("v2"));
    assert!(seen(&log).is_empty(), "overwriting a live entry is silent");
    assert_eq!(CacheEvict::evict(&mut c), 1);
    assert_eq!(
        seen(&log),
        vec![("second", "v2")],
        "evict reports the key bound by the most recent cache_set"
    );
}

#[test]
fn evict_on_an_all_live_cache_fires_nothing() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    assert_eq!(CacheEvict::evict(&mut c), 0);
    assert!(seen(&log).is_empty());
    assert_eq!(c.cache_evictions(), Some(0));
}

// --- retain() ------------------------------------------------------------------------

#[test]
fn retain_reports_stored_keys_on_both_the_predicate_and_expiry_limbs() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "drop_by_pred"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "keep"), Val::live("v2"));
    c.cache_set(TagKey::new(3, "drop_by_expiry"), Val::stale("v3"));

    // The predicate must also observe the stored key instances.
    let predicate_saw: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let probe = Arc::clone(&predicate_saw);
    c.retain(|k, _v| {
        probe.lock().unwrap().push(k.tag);
        k.id != 1
    });

    // MRU -> LRU is 3, 2, 1. The expired entry (3) is removed WITHOUT consulting `keep`.
    assert_eq!(
        &*predicate_saw.lock().unwrap(),
        &["keep", "drop_by_pred"],
        "the retain predicate sees stored keys, and is not consulted for expired entries"
    );
    assert_eq!(
        seen(&log),
        vec![("drop_by_expiry", "v3"), ("drop_by_pred", "v1")],
        "on_evict fires MRU -> LRU with each removed entry's stored key"
    );
    assert_eq!(c.cache_size(), 1);
    assert_eq!(c.cache_evictions(), Some(2));
}

#[test]
fn retain_keeping_everything_fires_nothing() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::live("v2"));
    c.retain(|_, _| true);
    assert!(seen(&log).is_empty());
    assert_eq!(c.cache_size(), 2);
}

#[test]
fn retain_dropping_everything_reports_every_stored_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::live("v2"));
    c.retain(|_, _| false);
    assert_eq!(seen(&log), vec![("k2", "v2"), ("k1", "v1")]);
    assert_eq!(c.cache_size(), 0);
}

// --- cache_clear_with_on_evict / cache_clear ------------------------------------------

#[test]
fn cache_clear_with_on_evict_reports_stored_keys_mru_to_lru() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "first"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::stale("v2"));
    // Rebind id 1's stored key; the overwrite also promotes it to MRU.
    c.cache_set(TagKey::new(1, "second"), Val::live("v1b"));
    c.cache_clear_with_on_evict();
    assert_eq!(
        seen(&log),
        vec![("second", "v1b"), ("k2", "v2")],
        "drain reports rebound stored keys, expired entries included, MRU -> LRU"
    );
    assert_eq!(c.cache_size(), 0);
    assert_eq!(c.cache_evictions(), Some(2));
}

#[test]
fn cache_clear_with_on_evict_leaves_the_cache_reusable() {
    // `drain_all` resets the LRU slab sentinels; a cache cleared this way must still
    // accept inserts and evict correctly afterwards.
    let (mut c, log) = cache_with_log(2);
    c.cache_set(TagKey::new(1, "a"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "b"), Val::live("v2"));
    c.cache_clear_with_on_evict();
    log.lock().unwrap().clear();

    c.cache_set(TagKey::new(3, "c"), Val::live("v3"));
    c.cache_set(TagKey::new(4, "d"), Val::live("v4"));
    c.cache_set(TagKey::new(5, "e"), Val::live("v5"));
    assert_eq!(c.cache_size(), 2);
    assert_eq!(
        seen(&log),
        vec![("c", "v3")],
        "post-drain inserts must still evict the true LRU with its stored key"
    );
    assert_eq!(
        c.cache_get(&TagKey::new(4, "?")).map(|v| v.label),
        Some("v4")
    );
    assert_eq!(
        c.cache_get(&TagKey::new(5, "?")).map(|v| v.label),
        Some("v5")
    );
}

#[test]
fn cache_clear_never_fires_on_evict() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "k2"), Val::stale("v2"));
    c.cache_clear();
    assert!(seen(&log).is_empty(), "cache_clear is silent");
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(c.cache_size(), 0);
}

// --- capacity shrink ------------------------------------------------------------------

#[test]
fn set_max_size_shrink_reports_stored_keys_lru_first() {
    let (mut c, log) = cache_with_log(4);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    c.cache_set(TagKey::new(2, "first"), Val::live("v2"));
    c.cache_set(TagKey::new(3, "k3"), Val::live("v3"));
    c.cache_set(TagKey::new(4, "k4"), Val::live("v4"));
    // Rebind id 2's stored key; the overwrite also promotes it to MRU.
    c.cache_set(TagKey::new(2, "second"), Val::live("v2b"));
    // Read 3 then 4 back to the front so id 2 is a shrink victim again.
    assert!(c.cache_get(&TagKey::new(3, "?")).is_some());
    assert!(c.cache_get(&TagKey::new(4, "?")).is_some());

    let previous = c.set_max_size(2);
    assert_eq!(previous, Some(4));
    // MRU -> LRU is 4, 3, 2, 1; shrinking to 2 evicts 1 then 2.
    assert_eq!(
        seen(&log),
        vec![("k1", "v1"), ("second", "v2b")],
        "shrink evicts LRU-first, reporting each victim's stored key"
    );
    assert_eq!(c.cache_size(), 2);
}

#[test]
fn try_set_max_size_zero_is_rejected_and_fires_nothing() {
    let (mut c, log) = cache_with_log(4);
    c.cache_set(TagKey::new(1, "k1"), Val::live("v1"));
    assert!(c.try_set_max_size(0).is_err());
    assert!(
        seen(&log).is_empty(),
        "a rejected shrink must evict nothing"
    );
    assert_eq!(c.cache_size(), 1);
}

// --- get_or_set family (sync) ---------------------------------------------------------

#[test]
fn get_or_set_with_over_expired_reports_the_old_stored_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    let v = c.cache_get_or_set_with(TagKey::new(1, "caller"), || Val::live("new"));
    assert_eq!(v.label, "new");
    assert_eq!(
        seen(&log),
        vec![("stored", "old")],
        "the replaced entry's OLD stored key must reach on_evict, not the caller's key"
    );
    assert_eq!(c.cache_evictions(), Some(1));
}

#[test]
fn get_or_set_with_over_expired_rebinds_the_stored_key_to_the_caller() {
    // Agreement with `cache_set`: after the replacement the caller's key is the stored one.
    let (mut c, _log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    let _ = c.cache_get_or_set_with(TagKey::new(1, "caller"), || Val::live("new"));
    let (k, _) = c
        .cache_remove_entry(&TagKey::new(1, "probe"))
        .expect("entry present");
    assert_eq!(k.tag, "caller");
}

#[test]
fn get_or_set_with_on_a_hit_fires_nothing_and_keeps_the_stored_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::live("v1"));
    let v = c.cache_get_or_set_with(TagKey::new(1, "caller"), || Val::live("unused"));
    assert_eq!(v.label, "v1", "the live value is returned, factory not run");
    assert!(seen(&log).is_empty());
    let (k, _) = c
        .cache_remove_entry(&TagKey::new(1, "probe"))
        .expect("entry present");
    assert_eq!(
        k.tag, "stored",
        "a hit must not rebind the stored key to the caller's instance"
    );
}

#[test]
fn get_or_set_with_new_key_at_capacity_reports_the_victims_stored_key() {
    let (mut c, log) = cache_with_log(1);
    c.cache_set(TagKey::new(1, "victim"), Val::live("v1"));
    let _ = c.cache_get_or_set_with(TagKey::new(2, "fresh"), || Val::live("v2"));
    assert_eq!(
        seen(&log),
        vec![("victim", "v1")],
        "the capacity victim is reported once with its own stored key"
    );
    assert_eq!(c.cache_evictions(), Some(1));
}

#[test]
fn try_get_or_set_with_ok_over_expired_reports_the_old_stored_key() {
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    let v: Result<&Val, ()> =
        c.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Ok(Val::live("new")));
    assert_eq!(v.unwrap().label, "new");
    assert_eq!(seen(&log), vec![("stored", "old")]);
    assert_eq!(c.cache_evictions(), Some(1));
}

#[test]
fn try_get_or_set_with_err_over_expired_fires_nothing_and_keeps_the_stored_key() {
    // On `Err` the store returns before `order.set` runs, so the expired entry -- and its
    // original stored key -- must survive untouched, with no on_evict and no eviction.
    let (mut c, log) = cache_with_log(8);
    c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
    let r: Result<&Val, &str> =
        c.cache_try_get_or_set_with(TagKey::new(1, "caller"), || Err("boom"));
    assert_eq!(r.err(), Some("boom"));
    assert!(seen(&log).is_empty(), "an Err factory must not evict");
    assert_eq!(c.cache_evictions(), Some(0));
    assert_eq!(c.cache_size(), 1, "the expired entry stays in place");

    let (k, v) = c
        .cache_remove_entry(&TagKey::new(1, "probe"))
        .expect("entry present");
    assert_eq!(
        k.tag, "stored",
        "an Err factory must not rebind the stored key"
    );
    assert_eq!(v.label, "old");
}

// --- get_or_set family (async) --------------------------------------------------------

#[cfg(feature = "async_core")]
mod async_paths {
    use super::*;
    use cached::CachedGetOrSetAsync;

    #[tokio::test]
    async fn async_get_or_set_with_over_expired_reports_the_old_stored_key() {
        let (mut c, log) = cache_with_log(8);
        c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
        let v = c
            .async_cache_get_or_set_with(TagKey::new(1, "caller"), || async { Val::live("new") })
            .await;
        assert_eq!(v.label, "new");
        assert_eq!(
            seen(&log),
            vec![("stored", "old")],
            "the async infallible path must also report the OLD stored key"
        );
        assert_eq!(c.cache_evictions(), Some(1));
        let (k, _) = c
            .cache_remove_entry(&TagKey::new(1, "probe"))
            .expect("entry present");
        assert_eq!(k.tag, "caller", "the replacement rebinds the stored key");
    }

    #[tokio::test]
    async fn async_get_or_set_with_on_a_hit_fires_nothing_and_keeps_the_stored_key() {
        let (mut c, log) = cache_with_log(8);
        c.cache_set(TagKey::new(1, "stored"), Val::live("v1"));
        let v = c
            .async_cache_get_or_set_with(TagKey::new(1, "caller"), || async { Val::live("unused") })
            .await;
        assert_eq!(v.label, "v1");
        assert!(seen(&log).is_empty());
        let (k, _) = c
            .cache_remove_entry(&TagKey::new(1, "probe"))
            .expect("entry present");
        assert_eq!(k.tag, "stored");
    }

    #[tokio::test]
    async fn async_get_or_set_with_new_key_at_capacity_reports_the_victims_stored_key() {
        let (mut c, log) = cache_with_log(1);
        c.cache_set(TagKey::new(1, "victim"), Val::live("v1"));
        let _ = c
            .async_cache_get_or_set_with(TagKey::new(2, "fresh"), || async { Val::live("v2") })
            .await;
        assert_eq!(seen(&log), vec![("victim", "v1")]);
    }

    #[tokio::test]
    async fn async_try_get_or_set_with_ok_over_expired_reports_the_old_stored_key() {
        let (mut c, log) = cache_with_log(8);
        c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
        let v: Result<&Val, ()> = c
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async {
                Ok(Val::live("new"))
            })
            .await;
        assert_eq!(v.unwrap().label, "new");
        assert_eq!(seen(&log), vec![("stored", "old")]);
        assert_eq!(c.cache_evictions(), Some(1));
    }

    #[tokio::test]
    async fn async_try_get_or_set_with_err_fires_nothing_and_keeps_the_stored_key() {
        let (mut c, log) = cache_with_log(8);
        c.cache_set(TagKey::new(1, "stored"), Val::stale("old"));
        let r: Result<&Val, &str> = c
            .async_cache_try_get_or_set_with(TagKey::new(1, "caller"), || async { Err("boom") })
            .await;
        assert_eq!(r.err(), Some("boom"));
        assert!(seen(&log).is_empty());
        assert_eq!(c.cache_evictions(), Some(0));
        let (k, v) = c
            .cache_remove_entry(&TagKey::new(1, "probe"))
            .expect("entry present");
        assert_eq!(k.tag, "stored");
        assert_eq!(v.label, "old");
    }
}
