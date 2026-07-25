//! Consumer-shaped coverage for `TtlSortedCache::set_with`, exercised only through the
//! crate's public API (as an external downstream consumer would use it).
//!
//! `TtlSortedSetBuilder` is re-exported at the crate root and from `cached::stores`
//! (`time_stores`-gated, like `TtlSortedCacheBuilder`), so a consumer can both chain the
//! returned value fluently and name the type in a signature. This file certifies both usage
//! patterns actually compile and work from outside the crate, which the in-crate
//! `#[cfg(test)]` module (same crate, same module privileges) cannot certify on its own.

#![cfg(feature = "time_stores")]

use cached::stores::TtlSortedCache;
use cached::time::Duration;
use cached::{CacheEvict, Cached};

/// The full one-line chain `set_with(k, v).ttl(..).evict().set()` compiles and behaves
/// correctly from outside the crate, using only `cached::{...}` public imports.
#[test]
fn chained_set_with_call_is_usable_from_the_public_api() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .max_size(1)
        .build()
        .unwrap();

    let displaced = cache
        .set_with(1u32, 10u32)
        .ttl(Duration::from_secs(5))
        .evict()
        .set();
    assert_eq!(displaced, None);
    assert_eq!(cache.cache_get(&1u32), Some(&10u32));

    // Exceeding the cap evicts the sole existing entry.
    let displaced = cache.set_with(2u32, 20u32).evict().set();
    assert_eq!(displaced, None);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(cache.cache_get(&1u32), None);
    assert_eq!(cache.cache_get(&2u32), Some(&20u32));
}

/// The builder value returned by `set_with` can be bound to a local — nameable via the
/// crate-root re-export `cached::TtlSortedSetBuilder` — and its setter methods called
/// across separate statements before the terminal `.set()`, not just as a single fluent
/// one-liner. A helper function taking the builder by name proves the type is nameable
/// in a downstream signature.
#[test]
fn set_with_builder_can_be_bound_and_configured_across_statements() {
    fn extend_ttl<'a, K: std::hash::Hash + Eq + Ord + Clone, V>(
        b: cached::TtlSortedSetBuilder<'a, K, V>,
    ) -> cached::TtlSortedSetBuilder<'a, K, V> {
        b.ttl(Duration::from_secs(60))
    }

    let mut cache: TtlSortedCache<&str, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(30))
        .build()
        .unwrap();

    let builder = cache.set_with("a", 1u32);
    let builder = extend_ttl(builder);
    let builder = builder.evict();
    let displaced = builder.set();

    assert_eq!(displaced, None);
    assert_eq!(cache.cache_get("a"), Some(&1u32));

    // The long .ttl() override means the entry outlives the cache's own short default TTL.
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_eq!(
        cache.cache_get("a"),
        Some(&1u32),
        "the builder-configured override must have taken effect"
    );
}

/// `set_with(..).set()` with no `.ttl()`/`.evict()` calls, from the public API, behaves
/// identically to plain `set` (the default-path parity guarantee, exercised as an external
/// consumer rather than via the in-crate unit test).
#[test]
fn set_with_default_path_matches_plain_set_via_public_api() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(200))
        .build()
        .unwrap();

    assert_eq!(cache.set_with(1u32, 10u32).set(), None);
    assert_eq!(cache.set(2u32, 20u32), None);
    assert_eq!(cache.cache_get(&1u32), Some(&10u32));
    assert_eq!(cache.cache_get(&2u32), Some(&20u32));

    // Both use the cache's default TTL and expire together.
    std::thread::sleep(std::time::Duration::from_millis(260));
    assert_eq!(cache.cache_get(&1u32), None);
    assert_eq!(cache.cache_get(&2u32), None);
}

/// `set_with(..).evict()` with no size limit configured runs a plain TTL sweep as part of
/// `.set()`, observable through the `CacheEvict`-independent `cache_evictions()` counter from
/// the public API (mirrors the in-crate `set_with_evict_triggers_eviction` case 1, exercised
/// here purely through public types/traits).
#[test]
fn set_with_evict_sweeps_expired_entries_via_public_api() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(10))
        .build()
        .unwrap();
    cache.set(1u32, 10u32);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(cache.cache_evictions(), Some(0));

    cache.set_with(2u32, 20u32).evict().set();
    assert_eq!(cache.cache_evictions(), Some(1));
    assert_eq!(cache.cache_get(&1u32), None);
    assert_eq!(cache.cache_get(&2u32), Some(&20u32));

    // The `CacheEvict::evict` trait method (also public) still works independently.
    assert_eq!(
        CacheEvict::evict(&mut cache),
        0,
        "already swept, nothing left to evict"
    );
}
