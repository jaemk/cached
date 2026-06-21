/*!
Tests that non-sharded stores (`UnboundCache`, `LruCache`) correctly accept a
custom `BuildHasher` via their `.hasher(...)` builder method.

These tests do NOT require a Redis server.

Covered:
- `UnboundCache<String, u64, MyHasher>` built via `.hasher(...)` inserts and reads
  back several entries, asserting correct hit/miss behavior.
- `LruCache<String, u64, MyHasher>` built via `.hasher(...)` does the same, and
  additionally exercises LRU eviction to confirm the hasher doesn't break ordering.
- A deterministic FNV-1a hasher is used as `MyHasher` so the custom-hasher path
  cannot silently fall back to `DefaultHashBuilder`.
*/

use std::hash::{BuildHasher, Hasher};

use cached::{Cached, LruCache, UnboundCache};

// ── Minimal FNV-1a BuildHasher ────────────────────────────────────────────────

/// A simple FNV-1a 64-bit hasher.
struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// A `BuildHasher` that constructs a `FnvHasher`.
#[derive(Clone, Default)]
struct FnvBuildHasher;

impl BuildHasher for FnvBuildHasher {
    type Hasher = FnvHasher;
    fn build_hasher(&self) -> Self::Hasher {
        FnvHasher::default()
    }
}

// ── UnboundCache with custom hasher ──────────────────────────────────────────

#[test]
fn unbound_cache_custom_hasher_hit_and_miss() {
    let mut cache = UnboundCache::<String, u64>::builder()
        .hasher(FnvBuildHasher)
        .build()
        .expect("build UnboundCache with FnvBuildHasher");

    // Miss on empty cache.
    assert_eq!(
        cache.cache_get(&"absent".to_string()),
        None,
        "cache miss on absent key"
    );

    // Insert several entries.
    cache.cache_set("alpha".to_string(), 1);
    cache.cache_set("beta".to_string(), 2);
    cache.cache_set("gamma".to_string(), 3);

    // All three must be retrievable.
    assert_eq!(cache.cache_get(&"alpha".to_string()), Some(&1));
    assert_eq!(cache.cache_get(&"beta".to_string()), Some(&2));
    assert_eq!(cache.cache_get(&"gamma".to_string()), Some(&3));

    // A key that was never inserted must still miss.
    assert_eq!(cache.cache_get(&"delta".to_string()), None);

    // Overwrite an existing entry.
    cache.cache_set("alpha".to_string(), 99);
    assert_eq!(
        cache.cache_get(&"alpha".to_string()),
        Some(&99),
        "overwritten value must be returned"
    );

    // Remove one entry; it must no longer be present.
    let _ = cache.cache_remove(&"beta".to_string());
    assert_eq!(
        cache.cache_get(&"beta".to_string()),
        None,
        "removed entry must return None"
    );
}

#[test]
fn unbound_cache_custom_hasher_metrics() {
    let mut cache = UnboundCache::<String, u64>::builder()
        .hasher(FnvBuildHasher)
        .build()
        .expect("build UnboundCache with FnvBuildHasher");

    // Baseline: no hits or misses yet.
    let hits_before = cache.cache_hits().unwrap_or(0);
    let misses_before = cache.cache_misses().unwrap_or(0);

    cache.cache_set("x".to_string(), 42);

    // Miss (absent key).
    cache.cache_get(&"absent".to_string());
    // Hit.
    cache.cache_get(&"x".to_string());

    let hits_after = cache.cache_hits().unwrap_or(0);
    let misses_after = cache.cache_misses().unwrap_or(0);

    assert!(
        hits_after > hits_before,
        "hit counter must have incremented"
    );
    assert!(
        misses_after > misses_before,
        "miss counter must have incremented"
    );
}

// ── LruCache with custom hasher ───────────────────────────────────────────────

#[test]
fn lru_cache_custom_hasher_hit_and_miss() {
    let mut cache = LruCache::<String, u64>::builder()
        .max_size(8)
        .hasher(FnvBuildHasher)
        .build()
        .expect("build LruCache with FnvBuildHasher");

    // Miss on empty cache.
    assert_eq!(cache.cache_get(&"absent".to_string()), None);

    cache.cache_set("a".to_string(), 10);
    cache.cache_set("b".to_string(), 20);
    cache.cache_set("c".to_string(), 30);

    assert_eq!(cache.cache_get(&"a".to_string()), Some(&10));
    assert_eq!(cache.cache_get(&"b".to_string()), Some(&20));
    assert_eq!(cache.cache_get(&"c".to_string()), Some(&30));
    assert_eq!(cache.cache_get(&"z".to_string()), None);
}

#[test]
fn lru_cache_custom_hasher_eviction() {
    // Capacity 2 means the third insert must evict the LRU entry.
    let mut cache = LruCache::<String, u64>::builder()
        .max_size(2)
        .hasher(FnvBuildHasher)
        .build()
        .expect("build LruCache(2) with FnvBuildHasher");

    cache.cache_set("first".to_string(), 1); // LRU
    cache.cache_set("second".to_string(), 2); // MRU

    // Access "first" to make "second" the LRU.
    cache.cache_get(&"first".to_string());

    // Third insert evicts the LRU, which is now "second".
    cache.cache_set("third".to_string(), 3);

    // "second" must have been evicted.
    assert_eq!(
        cache.cache_get(&"second".to_string()),
        None,
        "LRU entry (second) must have been evicted"
    );
    // The other two must still be present.
    assert_eq!(cache.cache_get(&"first".to_string()), Some(&1));
    assert_eq!(cache.cache_get(&"third".to_string()), Some(&3));

    // Eviction counter must reflect one eviction.
    assert_eq!(
        cache.cache_evictions(),
        Some(1),
        "eviction counter must be 1 after one LRU eviction"
    );
}
