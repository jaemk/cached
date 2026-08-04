//! Outside-in coverage for `retain` on the three map-backed sharded stores:
//! `ShardedUnboundCache`, `ShardedTtlCache`, and `ShardedExpiringCache`.
//!
//! Contract under test (mirroring the single-owner `retain` impls):
//!
//! - An entry survives exactly when the predicate returns `true`.
//! - The expiry-aware stores (`ShardedTtlCache`, `ShardedExpiringCache`) additionally remove
//!   every already-expired entry **regardless** of the predicate, and count each removal in
//!   `metrics().evictions`.
//! - `ShardedUnboundCache` has no expiry dimension and no eviction counter:
//!   `metrics().evictions` stays `None` across a `retain` that removes entries.
//! - Every removed entry fires `on_evict` exactly once, with the stored key and value.
//! - `retain` takes `&self`, so it is callable through any Arc-share clone of the handle.
//!
//! The sweeps here are built with `.shards(4)` and enough keys that several shards hold
//! entries (asserted via `shard_sizes()`), so removals are exercised across shard boundaries
//! rather than within a single map.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock, mpsc};

use cached::{Expires, ShardHasher, ShardedExpiringCache, ShardedUnboundCache};

#[cfg(feature = "time_stores")]
use cached::time::Duration;
#[cfg(feature = "time_stores")]
use cached::{ConcurrentCacheTtl, ShardedTtlCache};

/// Routes every key to shard 0 regardless of its value -- lets a test build a cache
/// with several empty shards plus a single "hot" shard holding every entry, so `retain`
/// can be exercised sweeping mostly-empty shards alongside one fully populated one.
#[derive(Clone, Default)]
struct ConstHasher;

impl ShardHasher<u32> for ConstHasher {
    fn shard_hash(&self, _key: &u32) -> u64 {
        0
    }
}

/// Number of shards currently holding at least one entry.
fn populated_shards(sizes: &[usize]) -> usize {
    sizes.iter().filter(|&&n| n > 0).count()
}

/// Recorder for `on_evict` firings: every call appends its `(key, value)` pair, so the
/// recorded length doubles as a "fired exactly once per removed entry" assertion.
#[derive(Clone)]
struct Fired(Arc<Mutex<Vec<(u32, u32)>>>);

impl Fired {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn sorted(&self) -> Vec<(u32, u32)> {
        let mut v = self.0.lock().expect("on_evict recorder poisoned").clone();
        v.sort_unstable();
        v
    }

    fn len(&self) -> usize {
        self.0.lock().expect("on_evict recorder poisoned").len()
    }
}

// ── ShardedUnboundCache ──────────────────────────────────────────────────────

#[test]
fn sharded_unbound_retain_filters_across_shards_and_fires_on_evict_once_per_entry() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedUnboundCache");

    for i in 0..32u32 {
        c.set(i, i * 10);
    }
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );

    // The predicate must see every stored entry with its stored key and value.
    let seen = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let seen2 = seen.clone();
    let removed: usize = c.retain(move |k, v| {
        seen2
            .lock()
            .expect("predicate recorder poisoned")
            .push((*k, *v));
        k % 2 == 0
    });
    // ShardedUnboundCache has no expiry dimension, so the returned count is exactly the
    // number of predicate rejections (the 16 odd keys).
    assert_eq!(removed, 16, "retain must return the removed count");
    let mut observed = seen.lock().expect("predicate recorder poisoned").clone();
    observed.sort_unstable();
    let all: Vec<(u32, u32)> = (0..32u32).map(|i| (i, i * 10)).collect();
    assert_eq!(
        observed, all,
        "the predicate must be applied to every entry exactly once"
    );

    // Survivors: exactly the even keys.
    assert_eq!(c.len(), 16);
    for i in 0..32u32 {
        if i % 2 == 0 {
            assert_eq!(c.peek(&i), Some(i * 10), "even key {i} must survive");
        } else {
            assert_eq!(c.peek(&i), None, "odd key {i} must be removed");
        }
    }

    // on_evict fired once per removed entry, with the stored (k, v).
    let expected: Vec<(u32, u32)> = (0..32u32)
        .filter(|i| i % 2 != 0)
        .map(|i| (i, i * 10))
        .collect();
    assert_eq!(fired.sorted(), expected);
    assert_eq!(
        fired.len(),
        16,
        "on_evict must fire exactly once per removal"
    );
}

#[test]
fn sharded_unbound_retain_keep_everything_removes_nothing() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedUnboundCache");
    for i in 0..32u32 {
        c.set(i, i);
    }
    let sizes_before = c.shard_sizes();
    assert!(populated_shards(&sizes_before) >= 2);

    c.retain(|_k, _v| true);

    assert_eq!(
        c.len(),
        32,
        "no expiry dimension: keep=true removes nothing"
    );
    assert_eq!(c.shard_sizes(), sizes_before);
    assert_eq!(fired.len(), 0, "nothing removed, so on_evict must not fire");
}

#[test]
fn sharded_unbound_retain_leaves_evictions_metric_none() {
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .build()
        .expect("build ShardedUnboundCache");
    for i in 0..32u32 {
        c.set(i, i);
    }
    assert_eq!(
        c.metrics().evictions,
        None,
        "ShardedUnboundCache never reports an eviction count"
    );

    c.retain(|k, _v| k % 4 == 0);

    assert_eq!(c.len(), 8);
    assert_eq!(
        c.metrics().evictions,
        None,
        "retain must not start reporting evictions on the unbounded store"
    );
}

#[test]
fn sharded_unbound_retain_is_visible_through_a_shared_clone_handle() {
    // `retain` takes `&self` (interior mutability), so it works through any Arc-share clone.
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .build()
        .expect("build ShardedUnboundCache");
    for i in 0..32u32 {
        c.set(i, i);
    }
    let handle = c.clone();
    handle.retain(|k, _v| *k < 4);

    assert_eq!(c.len(), 4, "the original handle observes the same store");
    assert_eq!(c.peek(&0), Some(0));
    assert_eq!(c.peek(&31), None);
}

// ── ShardedTtlCache ──────────────────────────────────────────────────────────

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_removes_expired_regardless_of_predicate() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_millis(30))
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache");

    for i in 0..16u32 {
        c.set(i, i * 10);
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: expired entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );

    // Long TTL for the live cohort.
    c.set_ttl(Duration::from_secs(3600));
    for i in 100..116u32 {
        c.set(i, i * 10);
    }
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    // Keep-everything predicate: every expired entry is still removed.
    c.retain(|_k, _v| true);

    assert_eq!(
        c.len(),
        16,
        "the 16 expired entries are swept despite keep=true"
    );
    for i in 100..116u32 {
        assert_eq!(c.peek(&i), Some(i * 10), "live key {i} must survive");
    }
    for i in 0..16u32 {
        assert_eq!(c.peek(&i), None, "expired key {i} must be gone");
    }
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before,
        16,
        "every expired removal counts as an eviction"
    );
    let expected: Vec<(u32, u32)> = (0..16u32).map(|i| (i, i * 10)).collect();
    assert_eq!(fired.sorted(), expected);
    assert_eq!(fired.len(), 16, "on_evict fires exactly once per removal");
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_predicate_removal_fires_on_evict_and_counts_evictions() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_secs(3600))
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache");
    for i in 0..32u32 {
        c.set(i, i * 10);
    }
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    c.retain(|_k, v| v % 20 == 0);

    assert_eq!(c.len(), 16);
    for i in 0..32u32 {
        let expected = if i % 2 == 0 { Some(i * 10) } else { None };
        assert_eq!(c.peek(&i), expected, "key {i} survivor mismatch");
    }
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before,
        16,
        "predicate removals count as evictions"
    );
    let expected: Vec<(u32, u32)> = (0..32u32)
        .filter(|i| i % 2 != 0)
        .map(|i| (i, i * 10))
        .collect();
    assert_eq!(fired.sorted(), expected);
    assert_eq!(fired.len(), 16, "on_evict fires exactly once per removal");
}

/// Callback path (an `on_evict` is configured, so this exercises the collect-under-lock /
/// fire-after-release branch, not the no-callback fast path): the returned count must fold
/// together entries removed for having already expired AND entries the predicate itself
/// rejected, in the same call. Neither cohort alone equals the total, so this is a genuine
/// check that `retain` isn't just returning one of the two categories.
#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_callback_return_count_includes_both_expired_and_predicate_rejected() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_millis(30))
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache");

    // 16 keys that will expire.
    for i in 0..16u32 {
        c.set(i, i * 10);
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: expired entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );

    // Long TTL for a second cohort, of which the predicate will reject the odd keys.
    c.set_ttl(Duration::from_secs(3600));
    for i in 100..116u32 {
        c.set(i, i * 10);
    }

    // 16 expired removals + 8 predicate-rejected removals (odd keys in 100..116) = 24 total,
    // which is neither 16 nor 8 alone.
    let removed: usize = c.retain(|k, _v| k % 2 == 0);
    assert_eq!(
        removed, 24,
        "retain's return must equal expired + predicate-rejected, not just one or the other"
    );
    assert_eq!(c.len(), 8, "expired entries swept, odd live keys filtered");
    assert_eq!(fired.len(), 24, "on_evict fires exactly once per removal");
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_keeps_never_expiring_entries() {
    // A disabled TTL stores `expires_at = None` (never expires): a keep-everything
    // retain must leave those entries alone no matter how much time has passed.
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_millis(30))
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache");
    c.set_ttl(Duration::ZERO); // disabled sentinel: entries never expire
    for i in 0..32u32 {
        c.set(i, i * 10);
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(populated_shards(&c.shard_sizes()) >= 2);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    c.retain(|_k, _v| true);

    assert_eq!(
        c.len(),
        32,
        "never-expiring entries survive a keep-everything retain"
    );
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions"),
        before,
        "nothing removed, so the eviction counter must not move"
    );
    assert_eq!(fired.len(), 0, "nothing removed, so on_evict must not fire");

    // The predicate still applies to never-expiring entries.
    c.retain(|k, _v| k % 2 == 0);
    assert_eq!(c.len(), 16);
    assert_eq!(c.peek(&0), Some(0));
    assert_eq!(c.peek(&1), None);
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before,
        16
    );
    assert_eq!(fired.len(), 16);
}

// No `on_evict` configured: exercises the in-place `HashMap::retain` fast path (no `Vec`
// collection, no key clones) added alongside `evict`'s existing no-callback fast path.
// Covers predicate filtering, expired-entry removal regardless of the predicate, and
// eviction counting, all without a callback to fire.
#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_no_callback_fast_path_filters_and_removes_expired() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_millis(30))
        .build()
        .expect("build ShardedTtlCache");

    for i in 0..16u32 {
        c.set(i, i * 10);
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: expired entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );

    // Long TTL for the live cohort; only even keys are kept by the predicate.
    c.set_ttl(Duration::from_secs(3600));
    for i in 100..116u32 {
        c.set(i, i * 10);
    }
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    let removed: usize = c.retain(|k, _v| k % 2 == 0);

    // The 16 expired entries are gone regardless of the predicate; among the 16 live
    // entries (100..116), only the even keys survive the predicate.
    assert_eq!(c.len(), 8, "expired entries swept, odd live keys filtered");
    for i in 0..16u32 {
        assert_eq!(c.peek(&i), None, "expired key {i} must be gone");
    }
    for i in 100..116u32 {
        let expected = if i % 2 == 0 { Some(i * 10) } else { None };
        assert_eq!(c.peek(&i), expected, "live key {i} survivor mismatch");
    }
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before,
        24,
        "16 expired + 8 predicate-filtered removals all count as evictions"
    );
    // The returned count folds together the 16 expired sweeps and the 8 predicate
    // rejections -- provably more than the predicate-rejection count alone (8), which
    // certifies the no-callback fast path's `usize` return, not just the eviction metric.
    assert_eq!(
        removed, 24,
        "retain's return must equal expired + predicate-rejected, not just one or the other"
    );
}

// ── ShardedExpiringCache ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
struct Val {
    v: u32,
    expired: bool,
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

fn live(v: u32) -> Val {
    Val { v, expired: false }
}

fn dead(v: u32) -> Val {
    Val { v, expired: true }
}

#[test]
fn sharded_expiring_retain_removes_expired_regardless_of_predicate() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache");

    // Even keys expired, odd keys live — both cohorts spread over the shards.
    for i in 0..32u32 {
        if i % 2 == 0 {
            c.set(i, dead(i * 10));
        } else {
            c.set(i, live(i * 10));
        }
    }
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    // Keep-everything predicate: expired values are removed anyway.
    c.retain(|_k, _v| true);

    assert_eq!(c.len(), 16, "expired values are swept despite keep=true");
    for i in 0..32u32 {
        let expected = if i % 2 == 0 { None } else { Some(live(i * 10)) };
        assert_eq!(c.peek(&i), expected, "key {i} survivor mismatch");
    }
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions")
            - before,
        16,
        "every expired removal counts as an eviction"
    );
    let expected: Vec<(u32, u32)> = (0..32u32)
        .filter(|i| i % 2 == 0)
        .map(|i| (i, i * 10))
        .collect();
    assert_eq!(fired.sorted(), expected);
    assert_eq!(fired.len(), 16, "on_evict fires exactly once per removal");
}

#[test]
fn sharded_expiring_retain_predicate_removal_fires_on_evict_and_counts_evictions() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache");
    for i in 0..32u32 {
        c.set(i, live(i * 10));
    }
    assert!(populated_shards(&c.shard_sizes()) >= 2);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    c.retain(|k, v| k % 2 == 0 && v.v < 200);

    assert_eq!(c.len(), 10, "keys 0,2,..,18 survive");
    for i in 0..32u32 {
        let expected = if i % 2 == 0 && i * 10 < 200 {
            Some(live(i * 10))
        } else {
            None
        };
        assert_eq!(c.peek(&i), expected, "key {i} survivor mismatch");
    }
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions")
            - before,
        22,
        "predicate removals count as evictions"
    );
    assert_eq!(fired.len(), 22, "on_evict fires exactly once per removal");
    let expected: Vec<(u32, u32)> = (0..32u32)
        .filter(|i| !(i % 2 == 0 && i * 10 < 200))
        .map(|i| (i, i * 10))
        .collect();
    assert_eq!(fired.sorted(), expected);
}

/// Callback path (an `on_evict` is configured, so this exercises the collect-under-lock /
/// fire-after-release branch, not the no-callback fast path): the returned count must fold
/// together entries removed for having already expired AND entries the predicate itself
/// rejected, in the same call. Neither cohort alone equals the total, so this is a genuine
/// check that `retain` isn't just returning one of the two categories.
#[test]
fn sharded_expiring_retain_callback_return_count_includes_both_expired_and_predicate_rejected() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache");

    // Even keys expired, odd keys live — both cohorts spread over the shards.
    for i in 0..32u32 {
        if i % 2 == 0 {
            c.set(i, dead(i * 10));
        } else {
            c.set(i, live(i * 10));
        }
    }
    assert!(populated_shards(&c.shard_sizes()) >= 2);

    // Predicate additionally rejects live keys below value 100 (i.e. i < 10, odd).
    let removed: usize = c.retain(|_k, v| v.v >= 100);

    // 16 expired (even) removals + 5 predicate-rejected live removals (odd i in 1,3,5,7,9) = 21.
    let expected_survivors = (0..32u32).filter(|i| i % 2 != 0 && i * 10 >= 100).count();
    let expected_removed = 32 - expected_survivors;
    assert_eq!(
        removed, expected_removed,
        "retain's return must equal expired + predicate-rejected, not just one or the other"
    );
    assert_eq!(c.len(), expected_survivors);
    assert_eq!(
        fired.len(),
        expected_removed,
        "on_evict fires exactly once per removal"
    );
}

#[test]
fn sharded_expiring_retain_keep_everything_keeps_live_entries() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache");
    for i in 0..32u32 {
        c.set(i, live(i));
    }
    let sizes_before = c.shard_sizes();
    assert!(populated_shards(&sizes_before) >= 2);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    c.retain(|_k, _v| true);

    assert_eq!(c.len(), 32);
    assert_eq!(c.shard_sizes(), sizes_before);
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions"),
        before,
        "nothing removed, so the eviction counter must not move"
    );
    assert_eq!(fired.len(), 0, "nothing removed, so on_evict must not fire");
}

#[test]
fn sharded_expiring_retain_is_visible_through_a_shared_clone_handle() {
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .build()
        .expect("build ShardedExpiringCache");
    for i in 0..32u32 {
        c.set(i, live(i));
    }
    let handle = c.clone();
    handle.retain(|k, _v| *k < 4);

    assert_eq!(c.len(), 4, "the original handle observes the same store");
    assert_eq!(c.peek(&0), Some(live(0)));
    assert_eq!(c.peek(&31), None);
}

// No `on_evict` configured: exercises the in-place `HashMap::retain` fast path (no `Vec`
// collection, no key clones) added alongside `evict`'s existing no-callback fast path.
// Covers predicate filtering, expired-entry removal regardless of the predicate, and
// eviction counting, all without a callback to fire.
#[test]
fn sharded_expiring_retain_no_callback_fast_path_filters_and_removes_expired() {
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .build()
        .expect("build ShardedExpiringCache");

    // Even keys expired, odd keys live — both cohorts spread over the shards.
    for i in 0..32u32 {
        if i % 2 == 0 {
            c.set(i, dead(i * 10));
        } else {
            c.set(i, live(i * 10));
        }
    }
    assert!(
        populated_shards(&c.shard_sizes()) >= 2,
        "cross-shard precondition: entries must span at least two shards, got {:?}",
        c.shard_sizes()
    );
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    // Predicate additionally filters out live keys below 100 (i.e. i < 10).
    let removed: usize = c.retain(|_k, v| v.v >= 100);

    // Expired (even) keys are gone regardless of the predicate; among the 16 live (odd)
    // keys, only those with value >= 100 (i >= 11, odd) survive the predicate.
    let expected_survivors = (0..32u32).filter(|i| i % 2 != 0 && i * 10 >= 100).count();
    assert_eq!(c.len(), expected_survivors);
    for i in 0..32u32 {
        let expected = if i % 2 != 0 && i * 10 >= 100 {
            Some(live(i * 10))
        } else {
            None
        };
        assert_eq!(c.peek(&i), expected, "key {i} survivor mismatch");
    }
    let expected_removed = 32 - expected_survivors;
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions")
            - before,
        expected_removed as u64,
        "expired + predicate-filtered removals all count as evictions"
    );
    // The returned count folds together the expired sweeps (even keys) and the
    // predicate-rejected live keys below value 100 -- provably more than the
    // predicate-rejection count alone, certifying the no-callback fast path's `usize`
    // return.
    assert_eq!(
        removed, expected_removed,
        "retain's return must equal expired + predicate-rejected, not just one or the other"
    );
}

// ── Zero-removal fast paths: empty cache, and a hot shard alongside empty shards ────

#[test]
fn sharded_unbound_retain_on_empty_cache_is_a_no_op() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedUnboundCache");
    assert_eq!(c.len(), 0);

    // Even a keep-nothing predicate has nothing to remove from an empty cache.
    c.retain(|_k, _v| false);

    assert_eq!(c.len(), 0);
    assert_eq!(
        fired.len(),
        0,
        "retain on an empty cache must not fire on_evict"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_on_empty_cache_is_a_no_op() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_secs(60))
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache");
    assert_eq!(c.len(), 0);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    c.retain(|_k, _v| false);

    assert_eq!(c.len(), 0);
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions"),
        before,
        "nothing to remove -- the eviction counter must not move"
    );
    assert_eq!(
        fired.len(),
        0,
        "retain on an empty cache must not fire on_evict"
    );
}

#[test]
fn sharded_expiring_retain_on_empty_cache_is_a_no_op() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache");
    assert_eq!(c.len(), 0);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    c.retain(|_k, _v| false);

    assert_eq!(c.len(), 0);
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions"),
        before,
        "nothing to remove -- the eviction counter must not move"
    );
    assert_eq!(
        fired.len(),
        0,
        "retain on an empty cache must not fire on_evict"
    );
}

#[test]
fn sharded_unbound_retain_over_single_hot_shard_leaves_empty_shards_untouched() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(8)
        .hasher(ConstHasher)
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedUnboundCache with ConstHasher");
    for i in 0..20u32 {
        c.set(i, i * 10);
    }
    let sizes_before = c.shard_sizes();
    assert_eq!(
        sizes_before[0], 20,
        "ConstHasher routes every key to shard 0"
    );
    assert_eq!(
        sizes_before[1..].iter().sum::<usize>(),
        0,
        "every other shard starts empty"
    );

    c.retain(|k, _v| k % 2 == 0);

    let sizes_after = c.shard_sizes();
    assert_eq!(
        sizes_after[0], 10,
        "half of the hot shard's entries survive"
    );
    assert_eq!(
        sizes_after[1..].iter().sum::<usize>(),
        0,
        "sweeping the empty shards must leave them empty"
    );
    assert_eq!(
        fired.len(),
        10,
        "on_evict fires once per removed entry, all from the single hot shard"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_over_single_hot_shard_leaves_empty_shards_untouched() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(8)
        .ttl(Duration::from_secs(3600))
        .hasher(ConstHasher)
        .on_evict(move |k: &u32, v: &u32| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, *v));
        })
        .build()
        .expect("build ShardedTtlCache with ConstHasher");
    for i in 0..20u32 {
        c.set(i, i * 10);
    }
    let sizes_before = c.shard_sizes();
    assert_eq!(
        sizes_before[0], 20,
        "ConstHasher routes every key to shard 0"
    );
    assert_eq!(sizes_before[1..].iter().sum::<usize>(), 0);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    c.retain(|k, _v| k % 2 == 0);

    let sizes_after = c.shard_sizes();
    assert_eq!(sizes_after[0], 10);
    assert_eq!(
        sizes_after[1..].iter().sum::<usize>(),
        0,
        "sweeping the empty shards must leave them empty"
    );
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before,
        10
    );
    assert_eq!(fired.len(), 10);
}

#[test]
fn sharded_expiring_retain_over_single_hot_shard_leaves_empty_shards_untouched() {
    let fired = Fired::new();
    let fired2 = fired.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(8)
        .hasher(ConstHasher)
        .on_evict(move |k: &u32, v: &Val| {
            fired2
                .0
                .lock()
                .expect("on_evict recorder poisoned")
                .push((*k, v.v));
        })
        .build()
        .expect("build ShardedExpiringCache with ConstHasher");
    for i in 0..20u32 {
        c.set(i, live(i * 10));
    }
    let sizes_before = c.shard_sizes();
    assert_eq!(
        sizes_before[0], 20,
        "ConstHasher routes every key to shard 0"
    );
    assert_eq!(sizes_before[1..].iter().sum::<usize>(), 0);
    let before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    c.retain(|k, _v| k % 2 == 0);

    let sizes_after = c.shard_sizes();
    assert_eq!(sizes_after[0], 10);
    assert_eq!(
        sizes_after[1..].iter().sum::<usize>(),
        0,
        "sweeping the empty shards must leave them empty"
    );
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions")
            - before,
        10
    );
    assert_eq!(fired.len(), 10);
}

// ── deep_clone interaction: retain through one handle must not touch a snapshot ────

#[test]
fn sharded_unbound_retain_through_original_does_not_affect_deep_clone_snapshot() {
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .build()
        .expect("build ShardedUnboundCache");
    for i in 0..32u32 {
        c.set(i, i * 10);
    }
    let snapshot = c.deep_clone();

    c.retain(|k, _v| *k < 4);

    assert_eq!(c.len(), 4, "the original handle is filtered by retain");
    assert_eq!(
        snapshot.len(),
        32,
        "the deep clone snapshot is untouched by retain on the original"
    );
    for i in 0..32u32 {
        assert_eq!(
            snapshot.peek(&i),
            Some(i * 10),
            "deep clone entry {i} must survive untouched"
        );
    }
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_through_original_does_not_affect_deep_clone_snapshot() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_secs(3600))
        .build()
        .expect("build ShardedTtlCache");
    for i in 0..32u32 {
        c.set(i, i * 10);
    }
    let snapshot = c.deep_clone();
    let snapshot_evictions_before = snapshot
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    c.retain(|k, _v| *k < 4);

    assert_eq!(c.len(), 4, "the original handle is filtered by retain");
    assert_eq!(
        snapshot.len(),
        32,
        "the deep clone snapshot is untouched by retain on the original"
    );
    assert_eq!(
        snapshot
            .metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions"),
        snapshot_evictions_before,
        "the snapshot's own eviction counter must not move when the original is retained"
    );
    for i in 0..32u32 {
        assert_eq!(
            snapshot.peek(&i),
            Some(i * 10),
            "deep clone entry {i} must survive untouched"
        );
    }
}

#[test]
fn sharded_expiring_retain_through_original_does_not_affect_deep_clone_snapshot() {
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .build()
        .expect("build ShardedExpiringCache");
    for i in 0..32u32 {
        c.set(i, live(i * 10));
    }
    let snapshot = c.deep_clone();

    c.retain(|k, _v| *k < 4);

    assert_eq!(c.len(), 4, "the original handle is filtered by retain");
    assert_eq!(
        snapshot.len(),
        32,
        "the deep clone snapshot is untouched by retain on the original"
    );
    for i in 0..32u32 {
        assert_eq!(
            snapshot.peek(&i),
            Some(live(i * 10)),
            "deep clone entry {i} must survive untouched"
        );
    }
}

// ── on_evict re-entrancy: the callback actually calls back into the same cache ─────
//
// The existing in-crate tests only assert every shard's write lock is *available*
// (`try_write().is_some()`) from inside `on_evict`. Here the callback goes further and
// actually re-enters with blocking `set`/`get`/`len` calls. `retain` runs on a background
// thread and the join is bounded by a timeout, so a regression turns into a clear
// assertion failure instead of an indefinite hang of the whole test binary.

#[test]
fn sharded_unbound_on_evict_reentry_from_retain_does_not_deadlock() {
    let handle: Arc<OnceLock<ShardedUnboundCache<u32, u32>>> = Arc::new(OnceLock::new());
    let handle2 = handle.clone();
    let reentries = Arc::new(AtomicU64::new(0));
    let reentries2 = reentries.clone();
    let c = ShardedUnboundCache::<u32, u32>::builder()
        .shards(4)
        .on_evict(move |k: &u32, _v: &u32| {
            let cache = handle2.get().expect("handle is set before retain runs");
            cache.set(9000 + *k, 1);
            assert_eq!(cache.get(&(9000 + *k)), Some(1));
            let _ = cache.len();
            reentries2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    handle.set(c.clone()).expect("handle set once");
    for i in 0..16u32 {
        c.set(i, i);
    }

    // Keep only the reentrant keys (>= 9000): a fresh reentrant key can land in a shard
    // this same retain sweep has not visited yet, and must survive that later visit
    // rather than being swept again (which would otherwise trigger a second, unbounded
    // round of reentrant on_evict firings for the newly-removed reentrant key).
    let (tx, rx) = mpsc::channel();
    let worker = c.clone();
    std::thread::spawn(move || {
        worker.retain(|k, _v| *k >= 9000);
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("retain with a re-entrant on_evict callback must not deadlock");

    assert_eq!(
        reentries.load(Ordering::Relaxed),
        16,
        "the re-entrant set/get/len calls must have run exactly once per removed entry"
    );
    assert_eq!(
        c.len(),
        16,
        "16 fresh reentrant keys were set from inside on_evict; the original 16 were removed"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_on_evict_reentry_from_retain_does_not_deadlock() {
    let handle: Arc<OnceLock<ShardedTtlCache<u32, u32>>> = Arc::new(OnceLock::new());
    let handle2 = handle.clone();
    let reentries = Arc::new(AtomicU64::new(0));
    let reentries2 = reentries.clone();
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(4)
        .ttl(Duration::from_secs(3600))
        .on_evict(move |k: &u32, _v: &u32| {
            let cache = handle2.get().expect("handle is set before retain runs");
            cache.set(9000 + *k, 1);
            assert_eq!(cache.get(&(9000 + *k)), Some(1));
            let _ = cache.len();
            reentries2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    handle.set(c.clone()).expect("handle set once");
    for i in 0..16u32 {
        c.set(i, i);
    }

    let (tx, rx) = mpsc::channel();
    let worker = c.clone();
    std::thread::spawn(move || {
        worker.retain(|k, _v| *k >= 9000);
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("retain with a re-entrant on_evict callback must not deadlock");

    assert_eq!(reentries.load(Ordering::Relaxed), 16);
    assert_eq!(c.len(), 16);
}

#[test]
fn sharded_expiring_on_evict_reentry_from_retain_does_not_deadlock() {
    let handle: Arc<OnceLock<ShardedExpiringCache<u32, Val>>> = Arc::new(OnceLock::new());
    let handle2 = handle.clone();
    let reentries = Arc::new(AtomicU64::new(0));
    let reentries2 = reentries.clone();
    let c = ShardedExpiringCache::<u32, Val>::builder()
        .shards(4)
        .on_evict(move |k: &u32, _v: &Val| {
            let cache = handle2.get().expect("handle is set before retain runs");
            cache.set(9000 + *k, live(1));
            assert_eq!(cache.get(&(9000 + *k)), Some(live(1)));
            let _ = cache.len();
            reentries2.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();
    handle.set(c.clone()).expect("handle set once");
    for i in 0..16u32 {
        c.set(i, live(i));
    }

    let (tx, rx) = mpsc::channel();
    let worker = c.clone();
    std::thread::spawn(move || {
        worker.retain(|k, _v| *k >= 9000);
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("retain with a re-entrant on_evict callback must not deadlock");

    assert_eq!(reentries.load(Ordering::Relaxed), 16);
    assert_eq!(c.len(), 16);
}

// ── Concurrency: retain racing set/get/remove/evict from other threads ─────────────
//
// Mirrors tests/v3_sharded_concurrent_expiry.rs: values are unique monotonically
// increasing ids, and `on_evict` asserts each id is only ever seen once (a HashSet
// insert that returns `false` means the same stored entry fired twice -- a double-fire
// under contention). For the eviction-tracking stores the total `on_evict` firings must
// also equal the total movement of `metrics().evictions` -- the two are bumped together
// on every internal removal path, so any divergence means retain raced another removal
// path (lazy expiry, `cache_remove`, `evict()`) into miscounting or double-firing.

#[test]
fn sharded_unbound_retain_races_concurrent_set_get_remove_without_double_fire() {
    const SHARDS: usize = 4;
    const KEYS: u32 = 16;
    const ROUNDS: u32 = 300;
    const WRITERS: usize = 4;
    const READERS: usize = 4;

    let next_id = Arc::new(AtomicU64::new(0));
    let fired_ids: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let fired_ids2 = fired_ids.clone();
    let c = Arc::new(
        ShardedUnboundCache::<u32, u64>::builder()
            .shards(SHARDS)
            .on_evict(move |_k: &u32, v: &u64| {
                let mut seen = fired_ids2.lock().expect("fired recorder poisoned");
                assert!(
                    seen.insert(*v),
                    "on_evict fired twice for the same stored value {v} -- \
                     double-fire under concurrent retain/remove"
                );
            })
            .build()
            .expect("build ShardedUnboundCache"),
    );

    let gate = Arc::new(Barrier::new(WRITERS + READERS + 1));
    let mut handles = Vec::new();

    for _ in 0..WRITERS {
        let c = c.clone();
        let gate = gate.clone();
        let next_id = next_id.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                c.set(k, id);
            }
        }));
    }
    for _ in 0..READERS {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let _ = c.get(&k);
                let _ = c.remove(&k);
            }
        }));
    }
    {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                c.retain(|k, _v| (*k + r) % 2 == 0);
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    // No torn shard: shard_sizes() (each acquired independently) must still sum to len().
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        c.len(),
        "post-race shard sizes must still sum to the total length"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_races_concurrent_set_get_evict_without_double_fire() {
    const SHARDS: usize = 4;
    const KEYS: u32 = 16;
    const ROUNDS: u32 = 150;
    const WRITERS: usize = 3;
    const READERS: usize = 3;

    let next_id = Arc::new(AtomicU64::new(0));
    let fired_ids: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let fired_count = Arc::new(AtomicU64::new(0));
    let fired_ids2 = fired_ids.clone();
    let fired_count2 = fired_count.clone();
    let c = Arc::new(
        ShardedTtlCache::<u32, u64>::builder()
            .shards(SHARDS)
            .ttl(Duration::from_millis(3))
            .on_evict(move |_k: &u32, v: &u64| {
                let mut seen = fired_ids2.lock().expect("fired recorder poisoned");
                assert!(
                    seen.insert(*v),
                    "on_evict fired twice for the same stored value {v}"
                );
                fired_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build ShardedTtlCache"),
    );
    let evictions_before = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");

    let gate = Arc::new(Barrier::new(WRITERS + READERS + 1));
    let mut handles = Vec::new();

    for _ in 0..WRITERS {
        let c = c.clone();
        let gate = gate.clone();
        let next_id = next_id.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                c.set(k, id);
            }
        }));
    }
    for _ in 0..READERS {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let _ = c.get(&k);
            }
        }));
    }
    {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                c.retain(|k, _v| (*k + r) % 2 == 0);
                let _ = c.evict();
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    let fired = fired_count.load(Ordering::Relaxed);
    let evictions_after = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");
    assert_eq!(
        fired,
        evictions_after - evictions_before,
        "on_evict must fire exactly once per counted eviction across the whole race, \
         no matter how retain/evict/cache_get interleaved"
    );
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        c.len(),
        "post-race shard sizes must still sum to the total length"
    );
}

#[test]
fn sharded_expiring_retain_races_concurrent_set_get_evict_without_double_fire() {
    const SHARDS: usize = 4;
    const KEYS: u32 = 16;
    const ROUNDS: u32 = 150;
    const WRITERS: usize = 3;
    const READERS: usize = 3;

    let next_id = Arc::new(AtomicU64::new(0));
    let fired_ids: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
    let fired_count = Arc::new(AtomicU64::new(0));
    let fired_ids2 = fired_ids.clone();
    let fired_count2 = fired_count.clone();
    let c = Arc::new(
        ShardedExpiringCache::<u32, Val>::builder()
            .shards(SHARDS)
            .on_evict(move |_k: &u32, v: &Val| {
                let mut seen = fired_ids2.lock().expect("fired recorder poisoned");
                assert!(
                    seen.insert(u64::from(v.v)),
                    "on_evict fired twice for the same stored value {}",
                    v.v
                );
                fired_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .expect("build ShardedExpiringCache"),
    );
    let evictions_before = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");

    let gate = Arc::new(Barrier::new(WRITERS + READERS + 1));
    let mut handles = Vec::new();

    for _ in 0..WRITERS {
        let c = c.clone();
        let gate = gate.clone();
        let next_id = next_id.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                // Roughly a third of inserts are born already-expired, so lazy-expiry
                // removal (via cache_get), evict(), and retain()'s forced expired-removal
                // all race to remove the very same entries.
                if id.is_multiple_of(3) {
                    c.set(k, dead(id as u32));
                } else {
                    c.set(k, live(id as u32));
                }
            }
        }));
    }
    for _ in 0..READERS {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let _ = c.get(&k);
            }
        }));
    }
    {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                c.retain(|k, _v| (*k + r) % 2 == 0);
                let _ = c.evict();
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    let fired = fired_count.load(Ordering::Relaxed);
    let evictions_after = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");
    assert_eq!(
        fired,
        evictions_after - evictions_before,
        "on_evict must fire exactly once per counted eviction across the whole race"
    );
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        c.len(),
        "post-race shard sizes must still sum to the total length"
    );
}

// ── Concurrency: the NO-callback retain fast path racing set/get/evict ─────────────
//
// The two race tests above build the cache WITH an `on_evict`, so they only exercise the
// callback branch of `retain` (collect-under-lock, fire-after-release). The no-callback
// fast path is a *different* code path: it drops filtered entries in place via
// `HashMap::retain` and computes the length delta (`before - after`) under the shard's
// write lock, with no `Vec` collection and no callback -- but the `evictions` counter
// itself is only incremented with `fetch_add` *after* that lock has been released, not
// while it is held. These tests drive that path under the
// same contention (concurrent set/get/evict) and certify:
//   * no torn shard / no lost entries -- `shard_sizes().sum() == len()` post-race, and
//   * exact eviction counting with no double-count -- a final, uncontended drain removes
//     every remaining entry and bumps `metrics().evictions` by exactly that many. A
//     `before - after` delta that ever over- or under-counted (e.g. if the subtraction
//     were moved outside the write lock, or the retain double-visited an entry) would
//     break this exact-count check.

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_no_callback_races_concurrent_set_get_evict() {
    const SHARDS: usize = 4;
    const KEYS: u32 = 16;
    const ROUNDS: u32 = 150;
    const WRITERS: usize = 3;
    const READERS: usize = 3;

    let next_id = Arc::new(AtomicU64::new(0));
    // No `on_evict`: this exercises the in-place `HashMap::retain` + length-delta fast path.
    let c = Arc::new(
        ShardedTtlCache::<u32, u64>::builder()
            .shards(SHARDS)
            .ttl(Duration::from_millis(3))
            .build()
            .expect("build ShardedTtlCache"),
    );

    let gate = Arc::new(Barrier::new(WRITERS + READERS + 1));
    let mut handles = Vec::new();

    for _ in 0..WRITERS {
        let c = c.clone();
        let gate = gate.clone();
        let next_id = next_id.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                c.set(k, id);
            }
        }));
    }
    for _ in 0..READERS {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let _ = c.get(&k);
            }
        }));
    }
    {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                c.retain(|k, _v| (*k + r) % 2 == 0);
                let _ = c.evict();
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    // No torn shard: shard_sizes() (each acquired independently) must still sum to len().
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        c.len(),
        "post-race shard sizes must still sum to the total length"
    );

    // Exact no-callback counting: sweep expired first (deterministic now that the workers
    // have joined), then a keep-nothing retain must remove exactly the live remainder and
    // bump the eviction counter by exactly that many -- no double-count, no undercount.
    c.retain(|_k, _v| true);
    let live = c.len();
    let before_drain = c
        .metrics()
        .evictions
        .expect("ShardedTtlCache tracks evictions");
    c.retain(|_k, _v| false);
    assert_eq!(c.len(), 0, "keep-nothing retain must empty the cache");
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedTtlCache tracks evictions")
            - before_drain,
        live as u64,
        "the no-callback fast path must count exactly one eviction per drained entry"
    );
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        0,
        "drained cache has empty shards"
    );
}

#[test]
fn sharded_expiring_retain_no_callback_races_concurrent_set_get_evict() {
    const SHARDS: usize = 4;
    const KEYS: u32 = 16;
    const ROUNDS: u32 = 150;
    const WRITERS: usize = 3;
    const READERS: usize = 3;

    let next_id = Arc::new(AtomicU64::new(0));
    // No `on_evict`: this exercises the in-place `HashMap::retain` + length-delta fast path.
    let c = Arc::new(
        ShardedExpiringCache::<u32, Val>::builder()
            .shards(SHARDS)
            .build()
            .expect("build ShardedExpiringCache"),
    );

    let gate = Arc::new(Barrier::new(WRITERS + READERS + 1));
    let mut handles = Vec::new();

    for _ in 0..WRITERS {
        let c = c.clone();
        let gate = gate.clone();
        let next_id = next_id.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                // Roughly a third of inserts are born already-expired, so lazy-expiry
                // removal (via cache_get), evict(), and retain()'s forced expired-removal
                // all race to remove the very same entries.
                if id.is_multiple_of(3) {
                    c.set(k, dead(id as u32));
                } else {
                    c.set(k, live(id as u32));
                }
            }
        }));
    }
    for _ in 0..READERS {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                let k = r % KEYS;
                let _ = c.get(&k);
            }
        }));
    }
    {
        let c = c.clone();
        let gate = gate.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            for r in 0..ROUNDS {
                c.retain(|k, _v| (*k + r) % 2 == 0);
                let _ = c.evict();
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    // No torn shard: shard_sizes() (each acquired independently) must still sum to len().
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        c.len(),
        "post-race shard sizes must still sum to the total length"
    );

    // Exact no-callback counting: sweep already-expired values first (deterministic now
    // that the workers have joined), then a keep-nothing retain must remove exactly the
    // live remainder and bump the eviction counter by exactly that many.
    c.retain(|_k, _v| true);
    let live = c.len();
    let before_drain = c
        .metrics()
        .evictions
        .expect("ShardedExpiringCache tracks evictions");
    c.retain(|_k, _v| false);
    assert_eq!(c.len(), 0, "keep-nothing retain must empty the cache");
    assert_eq!(
        c.metrics()
            .evictions
            .expect("ShardedExpiringCache tracks evictions")
            - before_drain,
        live as u64,
        "the no-callback fast path must count exactly one eviction per drained entry"
    );
    let sizes = c.shard_sizes();
    assert_eq!(
        sizes.iter().sum::<usize>(),
        0,
        "drained cache has empty shards"
    );
}

// ── ShardedTtlCache: refresh_on_hit and runtime set_ttl interacting with retain ─────

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_respects_refresh_on_hit_extended_expiry() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(2)
        .ttl(Duration::from_millis(150))
        .refresh_on_hit(true)
        .build()
        .expect("build ShardedTtlCache");
    c.set(1, 100);

    std::thread::sleep(std::time::Duration::from_millis(90));
    // A hit inside the original ttl window refreshes expires_at another 150ms out.
    assert_eq!(
        c.get(&1),
        Some(100),
        "hit within the original window must refresh the TTL"
    );

    std::thread::sleep(std::time::Duration::from_millis(90));
    // 180ms have elapsed since insert (> the original 150ms ttl) but only 90ms since
    // the refresh, so the entry must still be live and survive a retain sweep.
    c.retain(|_k, _v| true);
    assert_eq!(
        c.len(),
        1,
        "the refreshed entry must survive a retain sweep taken before its renewed expiry"
    );

    std::thread::sleep(std::time::Duration::from_millis(90));
    // Now 180ms have elapsed since the refresh (> the 150ms ttl): retain must sweep it.
    c.retain(|_k, _v| true);
    assert_eq!(
        c.len(),
        0,
        "the entry must be swept once its refreshed expiry has actually elapsed"
    );
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_uses_each_entrys_stored_expires_at_not_the_current_ttl_setting() {
    // `set_ttl` only changes the ttl applied to *future* inserts (`compute_expires_at`
    // reads the current ttl at insert time); it does not retroactively rearm entries
    // that are already stored. A retain call taken right after shortening the ttl must
    // still judge existing entries against their own already-computed expires_at.
    let c = ShardedTtlCache::<u32, u32>::builder()
        .shards(2)
        .ttl(Duration::from_secs(3600))
        .build()
        .expect("build ShardedTtlCache");
    c.set(1, 100);
    assert_eq!(c.ttl(), Some(Duration::from_secs(3600)));

    // Shorten the ttl drastically and let it lapse -- if retain (incorrectly) re-derived
    // expiry from the *current* ttl setting instead of the entry's own expires_at, the
    // entry would appear expired here.
    let prev = c.set_ttl(Duration::from_millis(1));
    assert_eq!(prev, Some(Duration::from_secs(3600)));
    std::thread::sleep(std::time::Duration::from_millis(30));

    c.retain(|_k, _v| true);
    assert_eq!(
        c.len(),
        1,
        "an entry inserted under the old long ttl must survive a retain taken after \
         shortening the ttl -- its own expires_at, not the new setting, governs expiry"
    );
    assert_eq!(c.get(&1), Some(100));

    // A freshly inserted entry, however, does pick up the new short ttl.
    c.set(2, 200);
    std::thread::sleep(std::time::Duration::from_millis(30));
    c.retain(|_k, _v| true);
    assert_eq!(c.len(), 1, "only key 1 (old ttl) should remain");
    assert_eq!(c.get(&1), Some(100), "key 1 (old ttl) unaffected");
    assert_eq!(
        c.get(&2),
        None,
        "key 2 (new short ttl) must have expired and been swept"
    );
}

// ── ConcurrentCachePeekAsync on the map-backed stores (specs/traits-concurrent.md CTRAIT-5,
// specs/design/0040-peek-is-an-in-memory-concept.md) ────────────────────────────────────────
//
// `tests/v3_concurrent_peek_async.rs` certifies the trait definition itself (no default body,
// the `async_peek` alias, the prelude re-export, `Send`) against a local store. This module
// certifies the concrete delegation on `ShardedUnboundCache`, `ShardedTtlCache`, and
// `ShardedExpiringCache`: `async_cache_peek` must agree with the sync `cache_peek` / inherent
// `peek`, with `Self::Error = Infallible`, and must not move the hit/miss metrics.

#[cfg(feature = "async_core")]
mod concurrent_peek_async {
    use super::*;
    use cached::{ConcurrentCacheBase, ConcurrentCachePeek, ConcurrentCachePeekAsync};

    #[tokio::test]
    async fn sharded_unbound_async_peek_matches_sync_peek_and_has_no_metrics_effect() {
        let c = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
        c.set(1, 10);

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            Some(10)
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &2)
                .await
                .unwrap(),
            None,
            "missing key peeks as None"
        );
        // Agrees with the sync side-effect-free peek.
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&c, &1).unwrap()
        );
        assert_eq!(
            ConcurrentCacheBase::cache_hits(&c),
            Some(0),
            "async peek must not record a hit"
        );
        assert_eq!(
            ConcurrentCacheBase::cache_misses(&c),
            Some(0),
            "async peek must not record a miss"
        );
    }

    #[cfg(feature = "time_stores")]
    #[tokio::test]
    async fn sharded_ttl_async_peek_matches_sync_peek_and_skips_expired() {
        let c = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_millis(30))
            .build()
            .expect("build ShardedTtlCache");
        c.set(1, 10);
        c.set(2, 20);
        std::thread::sleep(std::time::Duration::from_millis(80));

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            None,
            "an expired entry peeks as None, not a stale value"
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&c, &1).unwrap()
        );
        assert_eq!(
            c.len(),
            2,
            "peeking (sync or async) must not lazily remove the expired entry"
        );
        assert_eq!(ConcurrentCacheBase::cache_hits(&c), Some(0));
        assert_eq!(ConcurrentCacheBase::cache_misses(&c), Some(0));
    }

    #[tokio::test]
    async fn sharded_expiring_async_peek_matches_sync_peek_and_skips_expired() {
        let c = ShardedExpiringCache::<u32, Val>::builder()
            .build()
            .expect("build ShardedExpiringCache");
        c.set(1, live(10));
        c.set(2, dead(20));

        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            Some(live(10))
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &2)
                .await
                .unwrap(),
            None,
            "an already-expired value peeks as None"
        );
        assert_eq!(
            ConcurrentCachePeekAsync::async_cache_peek(&c, &1)
                .await
                .unwrap(),
            ConcurrentCachePeek::cache_peek(&c, &1).unwrap()
        );
        assert_eq!(
            c.len(),
            2,
            "peeking (sync or async) must not lazily remove the expired entry"
        );
        assert_eq!(ConcurrentCacheBase::cache_hits(&c), Some(0));
        assert_eq!(ConcurrentCacheBase::cache_misses(&c), Some(0));
    }
}
