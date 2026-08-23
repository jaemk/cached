/*!
Cross-store certification for `CacheExpiry`/`ConcurrentCacheExpiry::cache_peek_expires_at`
(issue #91).

Every implementing store already carries thorough in-file unit tests (named `peek_expires_at_*`)
in its own module under `src/stores`, pinning that store's own override against the
four-quadrant contract in isolation. What no single in-file test can show is that the SAME contract holds *uniformly*
across all nine implementors. This file exists for that: every property below is expressed once
as a generic function bound only by `CacheExpiry<K, V>` / `ConcurrentCacheExpiry<K, V>` (plus
whatever companion trait the property needs), then invoked with the identical body against every
applicable concrete store. A store whose override diverges from its siblings fails the shared
assertion, not a store-specific copy of it.

The file also only ever reaches the traits through `cached::prelude::*` (never a direct
`use cached::CacheExpiry`), so an export regression fails every test here, not just a dedicated
reachability check.

Properties certified:

  1. The four-quadrant return table -- `(None, None)` absent, `(Some(v), None)` present with no
     known deadline, `(Some(v), Some(future))` live, `(Some(v), Some(past))` expired-but-not-removed
     -- uniformly across all nine stores.
  2. Side-effect freedom, uniformly: hits/misses counters are unchanged by a peek of a present AND
     an absent key, and a peek of the LRU-tail key on an LRU-backed store does not save it from
     the next capacity eviction (i.e. does not promote recency).
  3. Agreement with `cache_peek_with_expiry_status`: on the TTL-based stores the returned deadline
     is in the past exactly when that method reports `true`.
  4. The `Expires`-store caveat, asserted rather than assumed: on the four `Expires`-based stores
     the deadline is advisory (from `Expires::expires_at`, default `None`), so an expired value
     that does not override `expires_at` reports `(Some(v), None)` -- `None` must never be read as
     proof of liveness. `is_expired` (via `cache_peek_with_expiry_status`) remains the authority.
  5. Reachability through `cached::prelude::*` and through a generic `T: CacheExpiry<K, V>` /
     `T: ConcurrentCacheExpiry<K, V>` bound.

Also certified where the store supports it: no deadline movement across two peeks with
`set_refresh_on_hit(true)` (TTL stores only -- `TtlSortedCache` deliberately has no
refresh-on-hit mode at all, so it is excluded from that one property).

Feature gating: the TTL stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`) require the `time_stores` feature, but the four `Expires`-based stores
(`ExpiringCache`, `ExpiringLruCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`) are
ungated. So gating is per-module, not a blanket `#![cfg(...)]` on the whole file: the
`expires_stores` module runs under every feature configuration, including zero features.
*/

use cached::prelude::*;
use cached::time::{Duration, Instant};
use std::hash::Hash;

// ── Shared fixture: the `Expires`-based stores' value type ────────────────────────────────────
//
// `is_expired` is driven by an explicit flag (deterministic, no sleeps needed), and `expires_at`
// is overridden only when a deadline is supplied. The same type produces every quadrant the
// `Expires`-store tests need, including the advisory-caveat shape: expired with no override.

#[derive(Clone, Debug, PartialEq)]
struct AdvisoryToken {
    payload: i32,
    expired: bool,
    deadline: Option<Instant>,
}

impl AdvisoryToken {
    fn live(payload: i32) -> Self {
        AdvisoryToken {
            payload,
            expired: false,
            deadline: None,
        }
    }

    fn live_with_deadline(payload: i32, deadline: Instant) -> Self {
        AdvisoryToken {
            payload,
            expired: false,
            deadline: Some(deadline),
        }
    }

    fn stale_no_deadline(payload: i32) -> Self {
        AdvisoryToken {
            payload,
            expired: true,
            deadline: None,
        }
    }

    fn stale_with_deadline(payload: i32, deadline: Instant) -> Self {
        AdvisoryToken {
            payload,
            expired: true,
            deadline: Some(deadline),
        }
    }
}

impl Expires for AdvisoryToken {
    fn is_expired(&self) -> bool {
        self.expired
    }

    fn expires_at(&self) -> Option<Instant> {
        self.deadline
    }
}

// ── Shared, family-agnostic generic helpers ────────────────────────────────────────────────────
//
// These are bound only by the common `Cached`/`ConcurrentCached` + `CacheExpiry`/
// `ConcurrentCacheExpiry` surface, so the identical body below is exercised against both the
// TTL-based and the `Expires`-based stores: the property genuinely holds crate-wide, not just
// within one store family.

/// Hits/misses counters, entry count, and eviction count are all identical before and after a
/// peek of a present key AND an absent key. The entry-count and eviction-counter checks matter on
/// their own: they are the only way this file would catch a peek that physically swept some
/// OTHER (unrelated) expired entry, or lazily removed the peeked one -- a mutation neither
/// counter alone would necessarily surface, and distinct from the "the peeked entry itself
/// survives" assertions the four-quadrant tests already make.
fn assert_side_effect_free_single_owner<K, V, C>(store: &mut C, present_key: &K, absent_key: &K)
where
    K: Hash + Eq,
    V: Clone,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_peek_expires_at(present_key);
    let _ = store.cache_peek_expires_at(absent_key);
    let _ = store.cache_peek_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a peek must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a peek must not change the miss counter"
    );
    assert_eq!(
        store.cache_size(),
        size0,
        "a peek must not change the entry count (no lazy removal of the peeked or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a peek must not increment the eviction counter"
    );
}

/// Concurrent counterpart of [`assert_side_effect_free_single_owner`].
fn assert_side_effect_free_concurrent<K, V, C>(store: &C, present_key: &K, absent_key: &K)
where
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size().unwrap();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_peek_expires_at(present_key);
    let _ = store.cache_peek_expires_at(absent_key);
    let _ = store.cache_peek_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a peek must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a peek must not change the miss counter"
    );
    assert_eq!(
        store.cache_size().unwrap(),
        size0,
        "a peek must not change the entry count (no lazy removal of the peeked or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a peek must not increment the eviction counter"
    );
}

/// A peek of the LRU-tail key must not promote its recency: with `max_size` 2, inserting `k1`
/// (LRU) then `k2` (MRU), peeking `k1`, then inserting `k3` must evict `k1` -- the still-LRU key
/// -- not `k2`. If the peek had promoted `k1`, `k2` would be evicted instead.
fn assert_peek_does_not_promote_lru_single_owner<K, V, C>(
    mut store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone + Hash + Eq,
    V: Clone + PartialEq + std::fmt::Debug,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    store.cache_set(k1.clone(), v1);
    store.cache_set(k2.clone(), v2.clone());

    // Peek the LRU key (k1). Must not promote it.
    let _ = store.cache_peek_expires_at(&k1);

    // Overflow: k3 must evict the still-LRU key (k1), not k2.
    store.cache_set(k3.clone(), v3.clone());

    assert_eq!(
        store.cache_get(&k1),
        None,
        "peek must not have promoted the LRU key; it must still be evicted"
    );
    assert_eq!(store.cache_get(&k2), Some(&v2));
    assert_eq!(store.cache_get(&k3), Some(&v3));
}

/// Concurrent counterpart of [`assert_peek_does_not_promote_lru_single_owner`].
fn assert_peek_does_not_promote_lru_concurrent<K, V, C>(
    store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone,
    V: Clone + PartialEq + std::fmt::Debug,
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let _ = store.cache_set(k1.clone(), v1).unwrap();
    let _ = store.cache_set(k2.clone(), v2.clone()).unwrap();

    let _ = store.cache_peek_expires_at(&k1);

    let _ = store.cache_set(k3.clone(), v3.clone()).unwrap();

    assert_eq!(
        store.cache_get(&k1).unwrap(),
        None,
        "peek must not have promoted the LRU key; it must still be evicted"
    );
    assert_eq!(store.cache_get(&k2).unwrap(), Some(v2));
    assert_eq!(store.cache_get(&k3).unwrap(), Some(v3));
}

/// Reachability through a bare generic bound: proves `CacheExpiry` is callable through nothing
/// but `T: CacheExpiry<K, V>`, independent of the concrete store type.
fn peek_via_generic_bound<K, V, C>(store: &C, key: &K) -> (Option<V>, Option<Instant>)
where
    K: Hash + Eq,
    V: Clone,
    C: CacheExpiry<K, V>,
{
    store.cache_peek_expires_at(key)
}

/// Concurrent counterpart of [`peek_via_generic_bound`].
fn peek_via_generic_bound_concurrent<K, V, C>(store: &C, key: &K) -> (Option<V>, Option<Instant>)
where
    C: ConcurrentCacheExpiry<K, V>,
{
    store.cache_peek_expires_at(key)
}

// ── TTL-based stores: TtlCache, LruTtlCache, TtlSortedCache, ShardedTtlCache, ────────────────
// ── ShardedLruTtlCache. Real deadlines; require `time_stores`. ────────────────────────────────

mod ttl_stores {
    #![cfg(feature = "time_stores")]

    use super::*;
    use cached::{LruTtlCache, ShardedLruTtlCache, ShardedTtlCache, TtlCache, TtlSortedCache};

    // A short-but-nonzero TTL the entry outlives within a "live" check, then a sleep past it to
    // force expiry. Kept small to keep the suite fast while staying deterministic.
    const SHORT_TTL: Duration = Duration::from_millis(80);
    const PAST_TTL: Duration = Duration::from_millis(160);
    // Used by tests that are not about expiry timing at all (side effects, LRU, reachability),
    // so there is no risk of an incidental expiry mid-test.
    const LONG_TTL: Duration = Duration::from_secs(5);

    /// The four-quadrant `CacheExpiry` contract, generic over any single-owner TTL store that
    /// exposes nothing but the common `Cached` + `CacheTtl` + `CacheExpiry` surface. Invoked with
    /// the identical body against all three single-owner TTL stores below.
    fn assert_four_quadrants<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheTtl + CacheExpiry<u32, i32>,
    {
        // (None, None): absent key.
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        // (Some(v), None): present, no known deadline (ttl disabled at insert time).
        let original_ttl = store.ttl();
        store.unset_ttl();
        store.cache_set(1, 100);
        assert_eq!(store.cache_peek_expires_at(&1u32), (Some(100), None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        // (Some(v), Some(future)): live.
        store.cache_set(2, 200);
        let (value, deadline) = store.cache_peek_expires_at(&2u32);
        assert_eq!(value, Some(200));
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        // (Some(v), Some(past)): expired, not removed.
        store.cache_set(3, 300);
        std::thread::sleep(PAST_TTL);
        let (value, deadline) = store.cache_peek_expires_at(&3u32);
        assert_eq!(value, Some(300));
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (Some(300), deadline),
            "an expired entry must survive the peek"
        );
    }

    /// Concurrent counterpart of [`assert_four_quadrants`].
    fn assert_four_quadrants_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32> + ConcurrentCacheTtl + ConcurrentCacheExpiry<u32, i32>,
    {
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        let original_ttl = store.ttl();
        store.unset_ttl();
        let _ = store.cache_set(1, 100).unwrap();
        assert_eq!(store.cache_peek_expires_at(&1u32), (Some(100), None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        let _ = store.cache_set(2, 200).unwrap();
        let (value, deadline) = store.cache_peek_expires_at(&2u32);
        assert_eq!(value, Some(200));
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        let _ = store.cache_set(3, 300).unwrap();
        std::thread::sleep(PAST_TTL);
        let (value, deadline) = store.cache_peek_expires_at(&3u32);
        assert_eq!(value, Some(300));
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (Some(300), deadline),
            "an expired entry must survive the peek"
        );
    }

    #[test]
    fn four_quadrants_uniform_across_single_owner_ttl_stores() {
        assert_four_quadrants(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn four_quadrants_uniform_across_sharded_ttl_stores() {
        assert_four_quadrants_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    /// No deadline movement across two peeks with `set_refresh_on_hit(true)`. `TtlSortedCache`
    /// deliberately does not implement `CacheRefreshOnHit` (its entries are deadline-ordered), so
    /// it is excluded here by construction (the bound would not be satisfiable).
    fn assert_peek_does_not_renew<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheExpiry<u32, i32> + CacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        store.cache_set(1, 100);

        let (_, first) = store.cache_peek_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_peek_expires_at(&1u32);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real read DOES renew, proving the assertion above is not vacuous.
        assert_eq!(store.cache_get(&1u32), Some(&100));
        let (_, after_hit) = store.cache_peek_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    /// Concurrent counterpart of [`assert_peek_does_not_renew`].
    fn assert_peek_does_not_renew_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32>
            + ConcurrentCacheExpiry<u32, i32>
            + ConcurrentCacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        let _ = store.cache_set(1, 100).unwrap();

        let (_, first) = store.cache_peek_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_peek_expires_at(&1u32);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        assert_eq!(store.cache_get(&1u32).unwrap(), Some(100));
        let (_, after_hit) = store.cache_peek_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    #[test]
    fn refresh_on_hit_no_renewal_uniform_across_single_owner_ttl_stores() {
        assert_peek_does_not_renew(
            TtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_peek_does_not_renew(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn refresh_on_hit_no_renewal_uniform_across_sharded_ttl_stores() {
        assert_peek_does_not_renew_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_peek_does_not_renew_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    /// The returned deadline is in the past exactly when `cache_peek_with_expiry_status` reports
    /// `true`. On these stores the deadline is a real clock reading (unlike the advisory
    /// `Expires`-based stores), so the two must never disagree.
    fn assert_deadline_matches_expiry_status<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheExpiry<u32, i32> + CloneCached<u32, i32>,
    {
        store.cache_set(1, 100);
        for _ in 0..2 {
            let (_, deadline) = store.cache_peek_expires_at(&1u32);
            let (_, expired) = store.cache_peek_with_expiry_status(&1u32);
            assert_eq!(
                deadline.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
            std::thread::sleep(PAST_TTL);
        }
    }

    /// Concurrent counterpart of [`assert_deadline_matches_expiry_status`].
    fn assert_deadline_matches_expiry_status_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32>
            + ConcurrentCacheExpiry<u32, i32>
            + ConcurrentCloneCached<u32, i32>,
    {
        let _ = store.cache_set(1, 100).unwrap();
        for _ in 0..2 {
            let (_, deadline) = store.cache_peek_expires_at(&1u32);
            let (_, expired) = store.cache_peek_with_expiry_status(&1u32);
            assert_eq!(
                deadline.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
            std::thread::sleep(PAST_TTL);
        }
    }

    #[test]
    fn deadline_agrees_with_expiry_status_uniform_across_single_owner_ttl_stores() {
        assert_deadline_matches_expiry_status(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn deadline_agrees_with_expiry_status_uniform_across_sharded_ttl_stores() {
        assert_deadline_matches_expiry_status_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn side_effect_free_uniform_across_single_owner_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut ttl, &1u32, &999u32);

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut lru_ttl, &1u32, &999u32);

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut sorted, &1u32, &999u32);
    }

    #[test]
    fn side_effect_free_uniform_across_sharded_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert_side_effect_free_concurrent(&ttl, &1u32, &999u32);

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert_side_effect_free_concurrent(&lru_ttl, &1u32, &999u32);
    }

    #[test]
    fn lru_no_promotion_uniform_across_single_owner_ttl_lru_store() {
        let store: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_single_owner(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    #[test]
    fn lru_no_promotion_uniform_across_sharded_ttl_lru_store() {
        let store: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_concurrent(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    /// Certifies a real, pre-existing cross-family DIVERGENCE rather than uniformity, found
    /// independently while certifying the single-owner stores
    /// (`peek_expires_at_overflowing_ttl_reports_no_deadline` in `src/stores/ttl.rs`,
    /// `peek_expires_at_reports_no_deadline_under_ttl_overflow` in `src/stores/lru_ttl.rs` and
    /// `src/stores/ttl_sorted.rs`) and the sharded stores
    /// (`peek_expires_at_extreme_ttl_is_clamped_not_overflowed` in `src/stores/sharded/ttl.rs`
    /// and `src/stores/sharded/lru_ttl.rs`) in isolation.
    ///
    /// On every other input in this file, `cache_peek_expires_at` agrees across the whole TTL
    /// family. `Duration::MAX` is the one input where it does not: the single-owner stores keep
    /// the raw `Duration` and let `Instant::checked_add` overflow to `None` (`compute_expires_at`
    /// -- indistinguishable, through this API alone, from "TTL disabled"), while the sharded
    /// stores store the ttl as an atomic `u64` of nanoseconds and clamp it to `u64::MAX` nanos
    /// (~584 years) BEFORE computing the deadline, so `checked_add` never sees an overflowing
    /// input and a real, very-distant deadline comes back instead.
    ///
    /// This is a pre-existing, deliberate representation difference (`Duration` field vs. atomic
    /// nanosecond clamp), not a bug, and this test does not change either side. It exists so that
    /// a future change collapsing the two representations -- and therefore this asymmetry --
    /// fails loudly here, in the one file whose entire premise is cross-store uniformity, instead
    /// of only in the two isolated per-family unit tests above.
    #[test]
    fn extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families() {
        // Single-owner family: Duration::MAX overflows Instant::checked_add, so the deadline
        // reports as (Some(v), None) -- the same shape as "TTL disabled".
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
        ttl.set_ttl(Duration::MAX);
        ttl.cache_set(1, 100);
        assert_eq!(
            ttl.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "TtlCache: an overflowing ttl must report no deadline, not a real one"
        );

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(SHORT_TTL)
            .build()
            .unwrap();
        lru_ttl.set_ttl(Duration::MAX);
        lru_ttl.cache_set(1, 100);
        assert_eq!(
            lru_ttl.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "LruTtlCache: an overflowing ttl must report no deadline, not a real one"
        );

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(SHORT_TTL).build().unwrap();
        sorted.set_ttl(Duration::MAX);
        sorted.cache_set(1, 100);
        assert_eq!(
            sorted.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "TtlSortedCache: an overflowing ttl must report no deadline, not a real one"
        );

        // Sharded family: the ttl is clamped to u64::MAX nanoseconds before the deadline is
        // computed, so it never overflows -- a real, ~584-year-out deadline comes back instead of
        // None. A century out is a conservative, sleep-free lower bound well short of the actual
        // ~584-year clamp.
        let a_century = Duration::from_secs(60 * 60 * 24 * 365 * 100);

        let sharded_ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(SHORT_TTL).build().unwrap();
        sharded_ttl.set_ttl(Duration::MAX);
        let _ = sharded_ttl.cache_set(1, 100).unwrap();
        let (value, deadline) = sharded_ttl.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedTtlCache: an overflowing ttl must clamp to a real, very-distant deadline, \
             not None -- unlike TtlCache"
        );

        let sharded_lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(SHORT_TTL)
            .build()
            .unwrap();
        sharded_lru_ttl.set_ttl(Duration::MAX);
        let _ = sharded_lru_ttl.cache_set(1, 100).unwrap();
        let (value, deadline) = sharded_lru_ttl.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedLruTtlCache: an overflowing ttl must clamp to a real, very-distant deadline, \
             not None -- unlike LruTtlCache"
        );
    }

    #[test]
    fn cache_expiry_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&ttl, &1u32).0, Some(100));

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&lru_ttl, &1u32).0, Some(100));

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&sorted, &1u32).0, Some(100));
    }

    #[test]
    fn concurrent_cache_expiry_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert_eq!(peek_via_generic_bound_concurrent(&ttl, &1u32).0, Some(100));

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&lru_ttl, &1u32).0,
            Some(100)
        );
    }
}

// ── `Expires`-based stores: ExpiringCache, ExpiringLruCache, ShardedExpiringCache, ────────────
// ── ShardedExpiringLruCache. Advisory deadlines; ungated. ─────────────────────────────────────

mod expires_stores {
    use super::*;
    use cached::{ExpiringCache, ExpiringLruCache, ShardedExpiringCache, ShardedExpiringLruCache};

    /// The four-quadrant `CacheExpiry` contract PLUS the advisory-caveat property (#4), generic
    /// over any single-owner `Expires`-based store. Invoked with the identical body against both
    /// single-owner `Expires` stores below.
    fn assert_four_quadrants_and_advisory_caveat<C>(mut store: C)
    where
        C: Cached<u32, AdvisoryToken>
            + CacheExpiry<u32, AdvisoryToken>
            + CloneCached<u32, AdvisoryToken>,
    {
        // (None, None): absent key.
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        // (Some(v), None): present, no known deadline -- the value does not override `expires_at`.
        store.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            store.cache_peek_expires_at(&1u32),
            (Some(AdvisoryToken::live(100)), None)
        );

        // (Some(v), Some(future)): live, with an overridden future deadline.
        let future = Instant::now() + Duration::from_secs(60);
        store.cache_set(2, AdvisoryToken::live_with_deadline(200, future));
        assert_eq!(
            store.cache_peek_expires_at(&2u32),
            (
                Some(AdvisoryToken::live_with_deadline(200, future)),
                Some(future)
            )
        );

        // (Some(v), Some(past)): present but expired, with an overridden past deadline; not
        // removed by the peek.
        let past = Instant::now() - Duration::from_millis(50);
        store.cache_set(3, AdvisoryToken::stale_with_deadline(300, past));
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            )
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            ),
            "an expired entry must survive the peek"
        );

        // The advisory caveat (#4): a value that IS expired (`is_expired() == true`) but does not
        // override `expires_at` must report `None`, not a false liveness signal. `is_expired`
        // (via `cache_peek_with_expiry_status`) remains the authority.
        store.cache_set(4, AdvisoryToken::stale_no_deadline(400));
        assert_eq!(
            store.cache_peek_expires_at(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), None),
            "an expired value with no expires_at override must report None, not proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
    }

    /// Concurrent counterpart of [`assert_four_quadrants_and_advisory_caveat`].
    fn assert_four_quadrants_and_advisory_caveat_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, AdvisoryToken>
            + ConcurrentCacheExpiry<u32, AdvisoryToken>
            + ConcurrentCloneCached<u32, AdvisoryToken>,
    {
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        let _ = store.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&1u32),
            (Some(AdvisoryToken::live(100)), None)
        );

        let future = Instant::now() + Duration::from_secs(60);
        let _ = store
            .cache_set(2, AdvisoryToken::live_with_deadline(200, future))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&2u32),
            (
                Some(AdvisoryToken::live_with_deadline(200, future)),
                Some(future)
            )
        );

        let past = Instant::now() - Duration::from_millis(50);
        let _ = store
            .cache_set(3, AdvisoryToken::stale_with_deadline(300, past))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            )
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            ),
            "an expired entry must survive the peek"
        );

        let _ = store
            .cache_set(4, AdvisoryToken::stale_no_deadline(400))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), None),
            "an expired value with no expires_at override must report None, not proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
    }

    #[test]
    fn four_quadrants_and_advisory_caveat_uniform_across_single_owner_expires_stores() {
        assert_four_quadrants_and_advisory_caveat(
            ExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_quadrants_and_advisory_caveat(
            ExpiringLruCache::<u32, AdvisoryToken>::builder()
                .max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn four_quadrants_and_advisory_caveat_uniform_across_sharded_expires_stores() {
        assert_four_quadrants_and_advisory_caveat_concurrent(
            ShardedExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_quadrants_and_advisory_caveat_concurrent(
            ShardedExpiringLruCache::<u32, AdvisoryToken>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn side_effect_free_uniform_across_single_owner_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_side_effect_free_single_owner(&mut plain, &1u32, &999u32);

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_side_effect_free_single_owner(&mut lru, &1u32, &999u32);
    }

    #[test]
    fn side_effect_free_uniform_across_sharded_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_side_effect_free_concurrent(&plain, &1u32, &999u32);

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_side_effect_free_concurrent(&lru, &1u32, &999u32);
    }

    #[test]
    fn lru_no_promotion_uniform_across_single_owner_expires_lru_store() {
        let store: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        assert_peek_does_not_promote_lru_single_owner(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    #[test]
    fn lru_no_promotion_uniform_across_sharded_expires_lru_store() {
        let store: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_concurrent(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    #[test]
    fn cache_expiry_reachable_via_prelude_and_generic_bound_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            peek_via_generic_bound(&plain, &1u32).0,
            Some(AdvisoryToken::live(100))
        );

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            peek_via_generic_bound(&lru, &1u32).0,
            Some(AdvisoryToken::live(100))
        );
    }

    #[test]
    fn concurrent_cache_expiry_reachable_via_prelude_and_generic_bound_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&plain, &1u32).0,
            Some(AdvisoryToken::live(100))
        );

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&lru, &1u32).0,
            Some(AdvisoryToken::live(100))
        );
    }
}
