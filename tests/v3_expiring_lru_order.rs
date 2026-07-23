//! Tests for `ExpiringLruCache::iter_order`, `key_order`, and `value_order`.
//!
//! Behavior pinned:
//! 1. The three methods return entries in most-to-least-recently-used order and
//!    their results agree with each other.
//! 2. Already-expired entries are excluded from all three methods while `len()`
//!    may still count them (lazy eviction).
//! 3. The return types are `Vec<(K, V)>`, `Vec<K>`, and `Vec<V>` respectively.

use cached::time::Instant;
use cached::{Cached, Expires, ExpiringLruCache};

// ── Value type with configurable expiry ──────────────────────────────────────

/// A simple value that can be configured to be live or expired.
#[derive(Clone, Debug, PartialEq)]
struct Val {
    id: u32,
    /// When true, `is_expired()` returns true regardless of any other state.
    expired: bool,
}

impl Val {
    fn live(id: u32) -> Self {
        Self { id, expired: false }
    }

    fn dead(id: u32) -> Self {
        Self { id, expired: true }
    }
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }

    fn expires_at(&self) -> Option<Instant> {
        None
    }
}

// ── 1 & 3. Order and return-type shape ────────────────────────────────────────

/// `iter_order`, `key_order`, and `value_order` return live entries in
/// most-to-least-recently-used order and agree with each other.
///
/// Access sequence that makes the order deterministic:
///   insert 1, 2, 3  (LRU order after inserts: 1 < 2 < 3)
///   get(1)          (1 becomes MRU)
///   get(2)          (2 becomes MRU, order: 3 < 1 < 2)
///
/// Expected MRU order: [2, 1, 3]
#[test]
fn iter_order_key_order_value_order_most_to_least_recent() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(10)
        .build()
        .expect("build ExpiringLruCache");

    // Insert three live entries.
    cache.cache_set(1, Val::live(1));
    cache.cache_set(2, Val::live(2));
    cache.cache_set(3, Val::live(3));

    // Access 1 then 2 to make 2 the MRU.
    let _ = cache.cache_get(&1); // 1 becomes MRU
    let _ = cache.cache_get(&2); // 2 becomes MRU

    // Expected order: 2 (MRU), 1, 3 (LRU)
    let expected_keys: Vec<u32> = vec![2, 1, 3];
    let expected_vals: Vec<Val> = expected_keys.iter().map(|k| Val::live(*k)).collect();
    let expected_pairs: Vec<(u32, Val)> = expected_keys
        .iter()
        .cloned()
        .zip(expected_vals.iter().cloned())
        .collect();

    // ── iter_order returns Vec<(K, CacheValue<V>)> ──────────────────────────
    let pairs: Vec<(u32, Val)> = cache
        .iter_order()
        .into_iter()
        .map(|(k, v)| (k, v.into_value()))
        .collect();
    assert_eq!(
        pairs, expected_pairs,
        "iter_order must return (key, value) pairs in MRU order"
    );

    // ── key_order returns Vec<K> ─────────────────────────────────────────────
    let keys: Vec<u32> = cache.key_order();
    assert_eq!(
        keys, expected_keys,
        "key_order must return keys in MRU order"
    );

    // ── value_order returns Vec<CacheValue<V>>; comparable against bare V ───
    let wrapped_vals = cache.value_order();
    assert_eq!(
        wrapped_vals, expected_vals,
        "value_order must return values in MRU order"
    );
    let vals: Vec<Val> = wrapped_vals.into_iter().map(|v| v.into_value()).collect();

    // ── all three methods agree with each other ──────────────────────────────
    let keys_from_pairs: Vec<u32> = pairs.iter().map(|(k, _)| *k).collect();
    let vals_from_pairs: Vec<Val> = pairs.into_iter().map(|(_, v)| v).collect();

    assert_eq!(
        keys, keys_from_pairs,
        "key_order and iter_order keys must agree"
    );
    assert_eq!(
        vals, vals_from_pairs,
        "value_order and iter_order values must agree"
    );
}

// ── 2. Expired entries are excluded; len() may still count them ───────────────

/// An expired entry is invisible to all three order methods but `len()` (lazy
/// eviction) may still count it.
#[test]
fn expired_entries_excluded_from_order_methods() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(10)
        .build()
        .expect("build ExpiringLruCache");

    cache.cache_set(1, Val::live(1));
    cache.cache_set(2, Val::dead(2)); // expired -- set directly as dead
    cache.cache_set(3, Val::live(3));

    // The cache stores 3 entries (lazy eviction: expired entry is still counted
    // by cache_size until a mutating operation removes it).
    let raw_size = cache.cache_size();
    assert!(
        raw_size >= 2,
        "cache_size should count stored entries (including expired); got {raw_size}"
    );

    // iter_order must exclude the expired entry.
    let pairs = cache.iter_order();
    let keys_in_pairs: Vec<u32> = pairs.iter().map(|(k, _)| *k).collect();
    assert!(
        !keys_in_pairs.contains(&2),
        "iter_order must exclude expired entry (key=2)"
    );
    assert!(
        keys_in_pairs.contains(&1) && keys_in_pairs.contains(&3),
        "iter_order must include live entries (keys 1 and 3)"
    );

    // key_order must exclude the expired entry.
    let keys: Vec<u32> = cache.key_order();
    assert!(
        !keys.contains(&2),
        "key_order must exclude expired entry (key=2)"
    );
    assert!(
        keys.contains(&1) && keys.contains(&3),
        "key_order must include live entries (keys 1 and 3)"
    );

    // value_order must exclude the expired entry (the wrapper Derefs to Val).
    let vals = cache.value_order();
    let val_ids: Vec<u32> = vals.iter().map(|v| v.id).collect();
    assert!(
        !val_ids.contains(&2),
        "value_order must exclude value of expired entry (id=2)"
    );
    assert!(
        val_ids.contains(&1) && val_ids.contains(&3),
        "value_order must include live values (ids 1 and 3)"
    );

    // All three return the same (live) count.
    assert_eq!(
        pairs.len(),
        keys.len(),
        "iter_order and key_order must have the same length"
    );
    assert_eq!(
        keys.len(),
        vals.len(),
        "key_order and value_order must have the same length"
    );
}

// ── Combined: expired entry excluded while len may be larger ──────────────────

/// When ALL inserted entries are expired, all three order methods return empty
/// vecs, but `cache_size` may still report the raw stored count.
#[test]
fn all_expired_returns_empty_vecs() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(10)
        .build()
        .expect("build ExpiringLruCache");

    cache.cache_set(1, Val::dead(1));
    cache.cache_set(2, Val::dead(2));

    assert!(cache.iter_order().is_empty());
    assert_eq!(cache.key_order(), Vec::<u32>::new());
    assert_eq!(cache.value_order(), Vec::<Val>::new());
}

// ── Single-entry edge case ────────────────────────────────────────────────────

#[test]
fn single_live_entry_appears_in_all_order_methods() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(5)
        .build()
        .expect("build ExpiringLruCache");

    cache.cache_set(7, Val::live(7));

    let pairs: Vec<(u32, Val)> = cache
        .iter_order()
        .into_iter()
        .map(|(k, v)| (k, v.into_value()))
        .collect();
    assert_eq!(pairs, vec![(7, Val::live(7))]);

    let keys: Vec<u32> = cache.key_order();
    assert_eq!(keys, vec![7]);

    // CacheValue compares directly against the bare value.
    assert_eq!(cache.value_order(), vec![Val::live(7)]);
}

// ── Empty cache ───────────────────────────────────────────────────────────────

#[test]
fn empty_cache_returns_empty_vecs() {
    let cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(5)
        .build()
        .expect("build ExpiringLruCache");

    assert!(cache.iter_order().is_empty());
    assert_eq!(cache.key_order(), Vec::<u32>::new());
    assert_eq!(cache.value_order(), Vec::<Val>::new());
}

// ── After capacity overflow (LRU eviction) ───────────────────────────────────

/// When the cache is full and a new entry is inserted, the LRU entry is evicted.
/// The order methods must reflect only the survivors and still report
/// most-to-least recently used order among them.
///
/// Setup (cap=2):
///   set(1), set(2)   → LRU order: 1 < 2
///   get(1)           → 1 becomes MRU; order: 2 < 1
///   set(3)           → evicts 2 (LRU); order after: 3 < 1  (3 is MRU as most recent insert)
///
/// Wait -- insertion of 3 promotes 3 to MRU, so order is: 3, 1
#[test]
fn order_methods_after_lru_eviction() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(2)
        .build()
        .expect("build ExpiringLruCache with cap=2");

    cache.cache_set(1, Val::live(1));
    cache.cache_set(2, Val::live(2));

    // Access key 1 to make it MRU. LRU order is now: 2 < 1.
    let _ = cache.cache_get(&1);

    // Insert key 3. Evicts key 2 (the LRU). LRU order after: 1 < 3.
    cache.cache_set(3, Val::live(3));

    // key 2 must be gone.
    let keys: Vec<u32> = cache.key_order();
    assert!(
        !keys.contains(&2),
        "key 2 must have been evicted; got keys: {keys:?}"
    );

    // Survivors are keys 1 and 3.
    assert!(
        keys.contains(&1) && keys.contains(&3),
        "keys 1 and 3 must be present; got: {keys:?}"
    );

    // Most-recently-used is key 3 (inserted last), then key 1.
    assert_eq!(
        keys,
        vec![3, 1],
        "order must be most-to-least recently used: [3, 1]; got: {keys:?}"
    );

    // iter_order, key_order, value_order must agree.
    let pairs = cache.iter_order();
    let vals = cache.value_order();

    assert_eq!(
        pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        keys,
        "iter_order keys must match key_order"
    );
    assert_eq!(
        vals.iter().map(|v| v.id).collect::<Vec<_>>(),
        keys,
        "value_order ids must match key_order"
    );
}

/// When a mix of live and expired entries are in the cache and capacity
/// overflow occurs, the order methods show only the live survivors.
#[test]
fn order_methods_after_eviction_with_pre_existing_expired_entry() {
    let mut cache: ExpiringLruCache<u32, Val> = ExpiringLruCache::builder()
        .max_size(3)
        .build()
        .expect("build ExpiringLruCache with cap=3");

    cache.cache_set(1, Val::live(1));
    cache.cache_set(2, Val::dead(2)); // expired
    cache.cache_set(3, Val::live(3));

    // Access key 1 to make it MRU.
    let _ = cache.cache_get(&1);

    // Insert key 4 to overflow capacity. LRU victim is key 2 (or 1, or 3 depending
    // on internal order after a dead-value get). Either way, key 4 must appear in
    // the live results and key 2 must never appear (it is expired).
    cache.cache_set(4, Val::live(4));

    let keys: Vec<u32> = cache.key_order();
    assert!(
        !keys.contains(&2),
        "expired key 2 must not appear in key_order; got: {keys:?}"
    );
    assert!(
        keys.contains(&4),
        "newly inserted key 4 must appear in key_order; got: {keys:?}"
    );

    // All three order methods must agree in length.
    let pairs = cache.iter_order();
    let vals = cache.value_order();
    assert_eq!(pairs.len(), keys.len());
    assert_eq!(vals.len(), keys.len());
}
