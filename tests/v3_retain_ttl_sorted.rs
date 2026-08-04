//! Consumer-shaped coverage for `TtlSortedCache::retain`, exercised only through the
//! crate's public API (as an external downstream consumer would use it).
//!
//! `retain` is a predicate filter plus an expiry sweep, distinct from the store's
//! `retain_latest(count, evict)` size trim. Because this store keeps a `HashMap` and an
//! expiry-ordered `BTreeSet` index in lockstep, the tests here also certify *post-retain
//! correctness* of the observable surface that reads that index (`evict`, `retain_latest`,
//! `len`/`iter`, and re-`set`): a stale or missing index entry would show up as a spurious
//! drop count or a resurrected entry.

#![cfg(feature = "time_stores")]

use cached::stores::{LruTtlCache, TtlCache, TtlSortedCache};
use cached::time::Duration;
use cached::{CacheEvict, CacheTtl, Cached, CachedIter};
use std::sync::{Arc, Mutex};

/// Recorded `(key, value)` pairs passed to `on_evict`.
type Events = Arc<Mutex<Vec<(u32, u32)>>>;

/// A cache whose `on_evict` appends every removed entry to the returned log.
fn events_cache(ttl: Duration) -> (TtlSortedCache<u32, u32>, Events) {
    let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events2 = events.clone();
    let cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(ttl)
        .on_evict(move |k: &u32, v: &u32| {
            events2.lock().unwrap().push((*k, *v));
        })
        .build()
        .unwrap();
    (cache, events)
}

/// Same as [`events_cache`] but with an entry-count cap, for the `size_limit`
/// interaction tests.
fn events_cache_capped(ttl: Duration, max_size: usize) -> (TtlSortedCache<u32, u32>, Events) {
    let events = Arc::new(Mutex::new(Vec::<(u32, u32)>::new()));
    let events2 = events.clone();
    let cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(ttl)
        .max_size(max_size)
        .on_evict(move |k: &u32, v: &u32| {
            events2.lock().unwrap().push((*k, *v));
        })
        .build()
        .unwrap();
    (cache, events)
}

fn fired(events: &Events) -> Vec<(u32, u32)> {
    let mut fired = events.lock().unwrap().clone();
    fired.sort_unstable();
    fired
}

fn sorted_keys(cache: &TtlSortedCache<u32, u32>) -> Vec<u32> {
    let mut keys: Vec<u32> = cache.iter().map(|(k, _v)| *k).collect();
    keys.sort_unstable();
    keys
}

/// The predicate decides which live entries survive; `retain` returns the number of
/// entries removed (design/0041-retain-returns-removed-count.md).
#[test]
fn retain_removes_entries_failing_the_predicate() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for k in 0u32..6 {
        cache.cache_set(k, k * 10);
    }

    // `retain` returns a `usize` count of removed entries: the explicit type annotation
    // below is a compile-time assertion of that signature.
    let removed: usize = cache.retain(|k, _v| k % 2 == 0);

    assert_eq!(removed, 3, "keys 1, 3, 5 were rejected by the predicate");
    assert_eq!(sorted_keys(&cache), vec![0, 2, 4]);
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(cache.cache_get(&3u32), None);
    assert_eq!(cache.cache_get(&4u32), Some(&40u32));
}

/// The returned count folds together predicate-rejected entries AND entries swept for
/// having already expired -- the two categories are not distinguished, so the count
/// provably differs from a caller's own tally of the predicate's `false` returns.
/// The count must agree with the `cache_size()` delta and with how many times
/// `on_evict` actually fired.
#[test]
fn retain_return_value_mixes_expired_sweeps_and_predicate_rejections() {
    let (mut cache, events) = events_cache(Duration::from_millis(20));
    // Two short-TTL entries: they expire during the sleep below and are swept
    // unconditionally, without ever reaching the predicate.
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    std::thread::sleep(std::time::Duration::from_millis(60));
    // Three long-lived entries inserted after the sleep, so they are still live when
    // `retain` runs. The predicate rejects two of them (3, 4) and keeps one (5).
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(60))
        .set();
    cache
        .set_with(4u32, 44u32)
        .ttl(Duration::from_secs(60))
        .set();
    cache
        .set_with(5u32, 55u32)
        .ttl(Duration::from_secs(60))
        .set();

    let before = cache.cache_size();
    let mut predicate_rejections = 0usize;
    let removed = cache.retain(|k, _v| {
        let keep = *k == 5;
        if !keep {
            predicate_rejections += 1;
        }
        keep
    });
    let after = cache.cache_size();

    // 2 expired (1, 2) + 2 predicate-rejected (3, 4) = 4, strictly more than the 2 the
    // predicate itself rejected -- proving the count is not recoverable from the
    // predicate's own return values alone.
    assert_eq!(removed, 4);
    assert_eq!(predicate_rejections, 2);
    assert_ne!(
        removed, predicate_rejections,
        "the returned count must differ from a tally of the predicate's own rejections"
    );
    assert_eq!(
        before - after,
        removed,
        "the returned count must agree with the cache_size() delta"
    );
    assert_eq!(
        events.lock().unwrap().len(),
        removed,
        "on_evict must fire exactly once per entry counted in the return value"
    );
    assert_eq!(sorted_keys(&cache), vec![5]);
}

/// The predicate sees the stored value, not just the key.
#[test]
fn retain_predicate_receives_key_and_value() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 200u32);
    cache.cache_set(3u32, 30u32);

    let mut seen: Vec<(u32, u32)> = Vec::new();
    cache.retain(|k, v| {
        seen.push((*k, *v));
        *v >= 100
    });
    seen.sort_unstable();

    assert_eq!(seen, vec![(1, 10), (2, 200), (3, 30)]);
    assert_eq!(sorted_keys(&cache), vec![2]);
}

/// `on_evict` fires exactly once per removed entry, with that entry's own key/value,
/// and never for a survivor.
#[test]
fn retain_fires_on_evict_once_per_removed_entry() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    cache.cache_set(3u32, 33u32);

    cache.retain(|k, _v| *k == 2);

    let mut fired = events.lock().unwrap().clone();
    fired.sort_unstable();
    assert_eq!(
        fired,
        vec![(1u32, 11u32), (3u32, 33u32)],
        "on_evict must fire once per removed entry with its own key/value"
    );
}

/// Every removal counts an eviction; a retain that removes nothing counts none.
#[test]
fn retain_increments_the_evictions_counter_per_removal() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for k in 0u32..5 {
        cache.cache_set(k, k);
    }
    let before = cache.cache_evictions().unwrap();

    cache.retain(|k, _v| *k >= 3);
    assert_eq!(
        cache.cache_evictions().unwrap() - before,
        3,
        "one eviction per removed entry"
    );

    let mid = cache.cache_evictions().unwrap();
    cache.retain(|_k, _v| true);
    assert_eq!(
        cache.cache_evictions().unwrap(),
        mid,
        "a retain that removes nothing must not count evictions"
    );
}

/// Expired entries are removed regardless of the predicate, and they still fire
/// `on_evict` and count an eviction.
#[test]
fn retain_removes_expired_entries_under_a_keep_everything_predicate() {
    let (mut cache, events) = events_cache(Duration::from_millis(20));
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    std::thread::sleep(std::time::Duration::from_millis(60));
    // Inserted after the sleep, so it is still live.
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(60))
        .set();
    let before = cache.cache_evictions().unwrap();

    cache.retain(|_k, _v| true);

    assert_eq!(sorted_keys(&cache), vec![3]);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(
        cache.cache_evictions().unwrap() - before,
        2,
        "each expired removal counts an eviction"
    );
    let mut fired = events.lock().unwrap().clone();
    fired.sort_unstable();
    assert_eq!(fired, vec![(1u32, 11u32), (2u32, 22u32)]);
}

/// Never-expiring entries (zero-TTL / overflowing-TTL inserts) survive a
/// keep-everything retain and are removed only when the predicate rejects them.
#[test]
fn retain_keeps_never_expiring_entries_under_a_keep_everything_predicate() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(20))
        .build()
        .unwrap();
    cache.set_with(1u32, 11u32).ttl(Duration::ZERO).set();
    cache.set_with(2u32, 22u32).ttl(Duration::MAX).set();
    // Expires during the sleep below.
    cache.cache_set(3u32, 33u32);
    std::thread::sleep(std::time::Duration::from_millis(60));

    cache.retain(|_k, _v| true);
    assert_eq!(
        sorted_keys(&cache),
        vec![1, 2],
        "never-expiring entries must survive the expiry sweep"
    );

    // They are still subject to the predicate.
    cache.retain(|k, _v| *k == 2);
    assert_eq!(sorted_keys(&cache), vec![2]);
}

/// After a retain that drops indexed entries, `evict()` must not report spurious drops.
/// A stale `BTreeSet` entry would be popped and counted even though its map entry is gone.
#[test]
fn evict_reports_no_spurious_drops_after_retain() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_millis(30))
        .build()
        .unwrap();
    for k in 0u32..6 {
        cache.cache_set(k, k);
    }
    cache.retain(|k, _v| *k >= 4);
    assert_eq!(cache.cache_size(), 2);

    // Nothing has expired yet: a correct index yields zero drops.
    assert_eq!(CacheEvict::evict(&mut cache), 0, "no drops before expiry");

    std::thread::sleep(std::time::Duration::from_millis(60));
    // Exactly the two survivors expire; the four retained-away entries must not be
    // counted a second time.
    assert_eq!(CacheEvict::evict(&mut cache), 2);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
}

/// `retain_latest` reads the same expiry index; after a retain it must trim exactly the
/// surviving entries and report the true drop count.
#[test]
fn retain_latest_reports_correct_count_after_retain() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for k in 0u32..6 {
        cache
            .set_with(k, k)
            .ttl(Duration::from_secs(60 + u64::from(k)))
            .set();
    }
    // Removes keys 0,1,2 from both the map and the index.
    cache.retain(|k, _v| *k >= 3);
    assert_eq!(cache.cache_size(), 3);

    // Three entries remain; trimming to 1 drops exactly 2 (the two soonest to expire).
    assert_eq!(cache.retain_latest(1, false), 2);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(
        sorted_keys(&cache),
        vec![5],
        "the latest-expiring survivor must be the one kept"
    );
    // Already within bounds: no further drops.
    assert_eq!(cache.retain_latest(1, false), 0);
}

/// `len` (via `cache_size`) and `iter` agree after a retain: no ghost entries in either
/// the map or the expiry-filtered view.
#[test]
fn len_and_iter_agree_after_retain() {
    let mut cache: TtlSortedCache<u32, u32> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for k in 0u32..8 {
        cache.cache_set(k, k * 2);
    }
    cache.retain(|k, _v| k % 3 == 0);

    let pairs: Vec<(u32, u32)> = {
        let mut p: Vec<(u32, u32)> = cache.iter().map(|(k, v)| (*k, *v)).collect();
        p.sort_unstable();
        p
    };
    assert_eq!(pairs, vec![(0, 0), (3, 6), (6, 12)]);
    assert_eq!(cache.cache_size(), pairs.len());
    // Nothing is expired, so a sweep changes neither count.
    assert_eq!(CacheEvict::evict(&mut cache), 0);
    assert_eq!(cache.cache_size(), pairs.len());
}

/// Re-inserting a key that `retain` dropped behaves like a fresh insert: no stale index
/// entry resurrects it or corrupts subsequent eviction accounting.
#[test]
fn resetting_a_retained_away_key_behaves_normally() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    cache.retain(|k, _v| *k == 2);
    assert_eq!(cache.cache_get(&1u32), None);
    let after_retain = cache.cache_evictions().unwrap();

    // Re-insert the dropped key: it is absent, so nothing is displaced.
    assert_eq!(cache.set(1u32, 111u32), None);
    assert_eq!(cache.cache_get(&1u32), Some(&111u32));
    assert_eq!(cache.cache_size(), 2);
    assert_eq!(
        cache.cache_evictions().unwrap(),
        after_retain,
        "a fresh insert over a retained-away key evicts nothing"
    );

    // Overwriting the live re-inserted value returns it and stays silent.
    assert_eq!(cache.set(1u32, 222u32), Some(111u32));
    assert_eq!(cache.cache_evictions().unwrap(), after_retain);

    // Only the original retain removal was reported.
    assert_eq!(events.lock().unwrap().clone(), vec![(1u32, 11u32)]);

    // The index is still sound: trimming to 1 drops exactly one entry.
    assert_eq!(cache.retain_latest(1, false), 1);
    assert_eq!(cache.cache_size(), 1);
}

/// Expired entries are removed *without consulting* `keep`: the predicate is never
/// invoked for them, so a predicate with side effects (or one that would have kept them)
/// cannot resurrect an expired entry.
#[test]
fn retain_does_not_consult_the_predicate_for_expired_entries() {
    let (mut cache, events) = events_cache(Duration::from_millis(20));
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    std::thread::sleep(std::time::Duration::from_millis(60));
    cache.set_ttl(Duration::from_secs(60));
    cache.cache_set(3u32, 33u32);

    let mut seen: Vec<u32> = Vec::new();
    cache.retain(|k, _v| {
        seen.push(*k);
        // Would keep everything, including the expired entries, if it were consulted.
        true
    });
    seen.sort_unstable();

    assert_eq!(
        seen,
        vec![3],
        "the predicate must only see live entries; expired ones are removed unconditionally"
    );
    assert_eq!(sorted_keys(&cache), vec![3]);
    assert_eq!(fired(&events), vec![(1, 11), (2, 22)]);
    assert_eq!(cache.cache_evictions().unwrap(), 2);
}

/// The clock is sampled once for the whole pass, so a slow predicate cannot cause
/// entries to be judged against different instants: entries that were live when
/// `retain` started survive even if their TTL elapses while the predicate is running.
#[test]
fn retain_samples_the_clock_once_for_the_whole_pass() {
    let (mut cache, events) = events_cache(Duration::from_millis(500));
    for k in 0u32..4 {
        cache.cache_set(k, k * 10);
    }

    let mut calls = 0usize;
    cache.retain(|_k, _v| {
        // Blow past every entry's TTL on the first call. With a per-entry clock read the
        // remaining three would be swept (and never even reach the predicate).
        if calls == 0 {
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        calls += 1;
        true
    });

    assert_eq!(calls, 4, "every entry is judged against the same instant");
    assert_eq!(cache.cache_size(), 4, "no entry is swept mid-pass");
    assert_eq!(cache.cache_evictions().unwrap(), 0);
    assert!(events.lock().unwrap().is_empty());

    // They are all expired by now, so the *next* retain sweeps them.
    cache.retain(|_k, _v| true);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_evictions().unwrap(), 4);
}

/// `retain` ignores `size_limit`: it never evicts to make room, and the cap is only
/// re-applied by the next insert -- which must pick the correct victim from the
/// *survivors*, not a phantom stamp left behind by the retain.
#[test]
fn retain_does_not_enforce_size_limit_and_the_next_set_evicts_the_right_victim() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 3);
    for k in 1u32..=3 {
        cache
            .set_with(k, k * 10)
            .ttl(Duration::from_secs(10 * u64::from(k)))
            .set();
    }

    // Sitting exactly at the cap: a keep-everything retain must not evict anything.
    cache.retain(|_k, _v| true);
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(
        cache.cache_evictions().unwrap(),
        0,
        "retain never enforces the size limit"
    );

    // Drop the soonest-to-expire entry. Its stamp sorts first in the expiry index, so a
    // stale one would be the phantom victim of the next size-limit eviction.
    cache.retain(|k, _v| *k != 1);
    assert_eq!(cache.cache_size(), 2);

    // Back to exactly the cap: still no eviction (the bound is `len > size_limit`).
    cache.set(4u32, 40u32);
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(cache.cache_evictions().unwrap(), 1);

    // Over the cap: the victim is key 2, the soonest-to-expire *survivor*.
    cache.set(5u32, 50u32);
    assert_eq!(cache.cache_size(), 3, "the cap is restored by the insert");
    assert_eq!(sorted_keys(&cache), vec![3, 4, 5]);
    assert_eq!(cache.cache_evictions().unwrap(), 2);
    assert_eq!(fired(&events), vec![(1, 10), (2, 20)]);

    // Updating an existing key at the cap still evicts nothing.
    assert_eq!(cache.set(3u32, 333u32), Some(30u32));
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(cache.cache_evictions().unwrap(), 2);
    assert_eq!(sorted_keys(&cache), vec![3, 4, 5]);
}

/// `cache_get_or_set_with_mut` inserts through the protected-eviction path (the
/// just-inserted entry is temporarily unlinked from the expiry index). Interleaving it
/// with `retain` must keep the new entry and evict the correct other entry.
#[test]
fn retain_interleaves_with_get_or_set_with_mut_protected_eviction() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 2);
    cache
        .set_with(1u32, 10u32)
        .ttl(Duration::from_secs(10))
        .set();
    cache
        .set_with(2u32, 20u32)
        .ttl(Duration::from_secs(20))
        .set();
    cache.retain(|k, _v| *k != 1);
    assert_eq!(cache.cache_size(), 1);

    // Below the cap: nothing is evicted, the factory result is returned by reference.
    {
        let v = cache.cache_get_or_set_with_mut(3u32, || 30u32);
        assert_eq!(*v, 30);
        *v += 1;
    }
    assert_eq!(cache.cache_size(), 2);
    assert_eq!(fired(&events), vec![(1, 10)]);

    // Over the cap: key 4 is protected, so key 2 (soonest to expire of the rest) goes.
    {
        let v = cache.cache_get_or_set_with_mut(4u32, || 40u32);
        assert_eq!(*v, 40);
    }
    assert_eq!(cache.cache_size(), 2);
    assert_eq!(sorted_keys(&cache), vec![3, 4]);
    assert_eq!(fired(&events), vec![(1, 10), (2, 20)]);
    assert_eq!(
        cache.cache_get(&3u32),
        Some(&31u32),
        "the mutation persisted"
    );

    // A hit does not re-insert or evict.
    assert_eq!(*cache.cache_get_or_set_with_mut(4u32, || 999u32), 40);
    assert_eq!(cache.cache_size(), 2);
    assert_eq!(cache.cache_evictions().unwrap(), 2);

    // The relinked stamp is still usable by the index-driven trim.
    assert_eq!(cache.retain_latest(0, false), 2);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
}

/// Same protected path via the fallible factory, plus the `Err` path: a failed factory
/// must leave both the entries and the counters exactly as `retain` left them.
#[test]
fn retain_interleaves_with_try_get_or_set_with_mut() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 2);
    cache
        .set_with(1u32, 10u32)
        .ttl(Duration::from_secs(10))
        .set();
    cache
        .set_with(2u32, 20u32)
        .ttl(Duration::from_secs(20))
        .set();
    cache.retain(|k, _v| *k != 1);

    let failed: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(9u32, || Err("boom"));
    assert_eq!(failed, Err("boom"));
    assert_eq!(cache.cache_size(), 1, "a failed factory inserts nothing");
    assert_eq!(cache.cache_evictions().unwrap(), 1);

    let ok: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(3u32, || Ok(30));
    assert_eq!(ok, Ok(&mut 30));
    let ok: Result<&mut u32, &str> = cache.cache_try_get_or_set_with_mut(4u32, || Ok(40));
    assert_eq!(ok, Ok(&mut 40));

    assert_eq!(cache.cache_size(), 2);
    assert_eq!(sorted_keys(&cache), vec![3, 4]);
    assert_eq!(fired(&events), vec![(1, 10), (2, 20)]);
    assert_eq!(cache.retain_latest(1, false), 1);
    assert_eq!(cache.cache_size(), 1);
}

/// A cache cloned after a retain must carry a self-consistent index: the clone's own
/// trims and sweeps report true counts, and the two caches are independent.
#[test]
fn clone_after_retain_is_independent_and_sound() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 0u32..6 {
        cache
            .set_with(k, k * 10)
            .ttl(Duration::from_secs(10 + u64::from(k)))
            .set();
    }
    cache.retain(|k, _v| k % 2 == 0);
    assert_eq!(cache.cache_evictions().unwrap(), 3);

    let mut clone = cache.clone();
    assert_eq!(sorted_keys(&clone), vec![0, 2, 4]);
    assert_eq!(
        clone.cache_evictions().unwrap(),
        3,
        "counters are snapshotted into the clone"
    );

    // The clone's index drives its own trim: three survivors, so trimming to 1 drops 2.
    assert_eq!(clone.retain_latest(1, false), 2);
    assert_eq!(sorted_keys(&clone), vec![4]);
    assert_eq!(CacheEvict::evict(&mut clone), 0);
    assert_eq!(clone.cache_evictions().unwrap(), 5);

    // The original is untouched and trims identically.
    assert_eq!(sorted_keys(&cache), vec![0, 2, 4]);
    assert_eq!(cache.cache_evictions().unwrap(), 3);
    assert_eq!(cache.retain_latest(1, false), 2);
    assert_eq!(sorted_keys(&cache), vec![4]);

    // A retain on the clone does not reach the original.
    clone.retain(|_k, _v| false);
    assert_eq!(clone.cache_size(), 0);
    assert_eq!(cache.cache_size(), 1);

    // `on_evict` is shared through the clone (it is an `Arc`), so the log holds the
    // original retain, both trims, and the clone's draining retain.
    assert_eq!(
        fired(&events),
        vec![
            (0, 0),
            (0, 0),
            (1, 10),
            (2, 20),
            (2, 20),
            (3, 30),
            (4, 40),
            (5, 50)
        ]
    );
}

/// Retaining an empty cache never calls the predicate and changes nothing.
#[test]
fn retain_on_an_empty_cache_is_a_noop() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));

    let mut calls = 0usize;
    cache.retain(|_k, _v| {
        calls += 1;
        false
    });

    assert_eq!(calls, 0);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_evictions().unwrap(), 0);
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(CacheEvict::evict(&mut cache), 0);
    assert_eq!(cache.retain_latest(0, true), 0);

    cache.cache_set(1u32, 11u32);
    assert_eq!(cache.cache_get(&1u32), Some(&11u32));
}

/// A predicate that rejects everything drains the cache completely (including
/// never-expiring entries) and leaves it reusable, with no phantom index entries to
/// inflate a later sweep or trim.
#[test]
fn retain_rejecting_everything_leaves_a_reusable_cache() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 0u32..4 {
        cache.cache_set(k, k);
    }
    cache.set_with(100u32, 1000u32).ttl(Duration::ZERO).set();

    cache.retain(|_k, _v| false);

    assert_eq!(cache.cache_size(), 0);
    assert_eq!(sorted_keys(&cache), Vec::<u32>::new());
    assert_eq!(cache.cache_evictions().unwrap(), 5);
    assert_eq!(events.lock().unwrap().len(), 5);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
    assert_eq!(cache.retain_latest(0, true), 0);
    assert_eq!(cache.retain_latest(5, false), 0);

    // A second draining retain on the drained cache is a no-op.
    cache.retain(|_k, _v| false);
    assert_eq!(cache.cache_evictions().unwrap(), 5);

    // Still fully usable.
    cache.cache_set(9u32, 99u32);
    assert_eq!(cache.cache_get(&9u32), Some(&99u32));
    assert_eq!(cache.retain_latest(0, false), 1);
    assert_eq!(cache.cache_size(), 0);
}

/// `cache_clear_with_on_evict` after a retain reports exactly the survivors -- the
/// retained-away entries must not be reported (or counted) a second time.
#[test]
fn cache_clear_with_on_evict_after_retain_reports_only_survivors() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 0u32..5 {
        cache.cache_set(k, k * 10);
    }
    cache.retain(|k, _v| *k >= 3);
    assert_eq!(cache.cache_evictions().unwrap(), 3);

    cache.cache_clear_with_on_evict();

    assert_eq!(
        cache.cache_evictions().unwrap(),
        5,
        "only the two survivors are reported by the clear"
    );
    assert_eq!(
        fired(&events),
        vec![(0, 0), (1, 10), (2, 20), (3, 30), (4, 40)]
    );
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
    assert_eq!(cache.retain_latest(0, true), 0);

    cache.cache_set(7u32, 77u32);
    assert_eq!(cache.cache_get(&7u32), Some(&77u32));
}

/// `cache_reset` after a retain clears the entries and the metrics without firing
/// `on_evict`, and leaves a working cache.
#[test]
fn cache_reset_after_retain_clears_entries_and_metrics() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 0u32..4 {
        cache.cache_set(k, k * 10);
    }
    cache.retain(|k, _v| *k == 0);
    assert_eq!(cache.cache_get(&0u32), Some(&0u32));

    cache.cache_reset();

    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
    assert_eq!(cache.cache_evictions(), Some(0));
    assert_eq!(sorted_keys(&cache), Vec::<u32>::new());
    assert_eq!(
        events.lock().unwrap().len(),
        3,
        "reset fires no callbacks of its own"
    );
    assert_eq!(CacheEvict::evict(&mut cache), 0);

    cache.cache_set(1u32, 11u32);
    assert_eq!(cache.cache_get(&1u32), Some(&11u32));
    assert_eq!(cache.retain_latest(0, false), 1);
}

/// Entries inserted through the `set_with(..).ttl(..)` builder (per-entry TTL override,
/// including the never-expiring zero TTL) must be indexed and retained correctly, and a
/// later `set_with(..).evict().set()` must sweep exactly the entries that expired.
#[test]
fn retain_with_per_entry_ttl_builder_and_opt_in_evict() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 1u32..=3 {
        cache
            .set_with(k, k * 11)
            .ttl(Duration::from_millis(40))
            .set();
    }
    cache.set_with(100u32, 1000u32).ttl(Duration::ZERO).set();
    cache
        .set_with(4u32, 44u32)
        .ttl(Duration::from_secs(60))
        .set();

    // Remove one short-TTL entry: its stamp sorts inside the sweep range below, so a
    // stale one would be popped and counted as a drop by the sweep.
    cache.retain(|k, _v| *k != 1);
    assert_eq!(cache.cache_size(), 4);
    assert_eq!(cache.cache_evictions().unwrap(), 1);

    std::thread::sleep(std::time::Duration::from_millis(80));

    // Opt-in eviction on the builder path: with no size limit this runs the expiry sweep.
    cache
        .set_with(5u32, 55u32)
        .ttl(Duration::from_secs(60))
        .evict()
        .set();

    assert_eq!(sorted_keys(&cache), vec![4, 5, 100]);
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(
        cache.cache_evictions().unwrap(),
        3,
        "only keys 2 and 3 expired; key 1 was already removed by the retain"
    );
    assert_eq!(fired(&events), vec![(1, 11), (2, 22), (3, 33)]);
    assert_eq!(
        CacheEvict::evict(&mut cache),
        0,
        "no stale stamps remain after the builder sweep"
    );
}

/// Shrinking the bound after a retain goes through the same expiry index: it must drop
/// exactly the overflow, keeping the latest-expiring survivors.
#[test]
fn set_max_size_after_retain_evicts_exactly_the_overflow() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    for k in 0u32..5 {
        cache
            .set_with(k, k * 10)
            .ttl(Duration::from_secs(10 + u64::from(k)))
            .set();
    }
    cache.retain(|k, _v| *k >= 2);
    assert_eq!(cache.cache_size(), 3);

    assert_eq!(
        cache.set_max_size(1),
        None,
        "no previous bound was configured"
    );

    assert_eq!(cache.cache_size(), 1);
    assert_eq!(sorted_keys(&cache), vec![4]);
    assert_eq!(cache.capacity(), Some(1));
    assert_eq!(cache.cache_evictions().unwrap(), 4);
    assert_eq!(fired(&events), vec![(0, 0), (1, 10), (2, 20), (3, 30)]);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
}

/// Non-`Copy` keys exercise the `Arc`-shared key stored in the expiry index; the
/// predicate sees the stored key/value pair and removals resolve to the right entries.
#[test]
fn retain_with_non_copy_keys_removes_the_right_entries() {
    let mut cache: TtlSortedCache<String, String> = TtlSortedCache::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    for name in ["alpha", "beta", "gamma", "delta"] {
        cache.cache_set(name.to_string(), name.to_uppercase());
    }

    cache.retain(|k, v| {
        assert_eq!(
            *v,
            k.to_uppercase(),
            "the stored pair reaches the predicate"
        );
        k.len() == 5
    });

    let mut keys: Vec<String> = cache.iter().map(|(k, _v)| k.clone()).collect();
    keys.sort();
    assert_eq!(keys, vec!["alpha", "delta", "gamma"]);
    assert_eq!(cache.cache_get("beta"), None);
    assert_eq!(cache.cache_size(), 3);
    assert_eq!(cache.retain_latest(1, false), 2);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
}

/// Removing a key that `retain` already dropped is a no-op: no value, no callback, no
/// eviction counted -- and the surviving entries still remove normally.
#[test]
fn cache_remove_of_a_retained_away_key_is_a_noop() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    cache.cache_set(1u32, 11u32);
    cache.cache_set(2u32, 22u32);
    cache.cache_set(3u32, 33u32);
    cache.retain(|k, _v| *k != 2);

    assert_eq!(cache.cache_remove(&2u32), None);
    assert_eq!(cache.cache_evictions().unwrap(), 1);
    assert_eq!(fired(&events), vec![(2, 22)]);

    assert_eq!(cache.cache_remove(&1u32), Some(11u32));
    assert_eq!(cache.cache_evictions().unwrap(), 2);
    assert_eq!(fired(&events), vec![(1, 11), (2, 22)]);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(CacheEvict::evict(&mut cache), 0);
    assert_eq!(cache.retain_latest(0, false), 1);
}

/// Runs one identical scenario -- two entries that expire, one live entry kept by the
/// predicate, one live entry rejected -- against a store, yielding
/// `(keys the predicate saw, survivors, evictions delta, on_evict log)`.
macro_rules! retain_parity_case {
    ($cache:expr, $events:expr) => {{
        let cache = &mut $cache;
        cache.cache_set(1u32, 11u32);
        cache.cache_set(2u32, 22u32);
        std::thread::sleep(std::time::Duration::from_millis(60));
        cache.set_ttl(Duration::from_secs(60));
        cache.cache_set(3u32, 33u32);
        cache.cache_set(4u32, 44u32);
        assert_eq!(cache.cache_evictions(), Some(0));

        let mut seen: Vec<u32> = Vec::new();
        cache.retain(|k, _v| {
            seen.push(*k);
            *k != 4
        });
        seen.sort_unstable();

        let evictions = cache.cache_evictions().unwrap();
        let survivors: Vec<u32> = (1u32..=4)
            .filter(|k| cache.cache_get(k).is_some())
            .collect();
        (seen, survivors, evictions, fired($events))
    }};
}

/// The documented parity claim: `TtlSortedCache::retain` behaves exactly like
/// `TtlCache::retain` and `LruTtlCache::retain` -- expired entries are removed without
/// consulting the predicate, and every removal (expired or rejected) fires `on_evict`
/// and counts an eviction exactly once.
#[test]
fn retain_matches_ttl_cache_and_lru_ttl_cache() {
    let sorted_events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = sorted_events.clone();
    let mut sorted = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, v: &u32| sink.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    let ttl_events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = ttl_events.clone();
    let mut ttl = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, v: &u32| sink.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    let lru_events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = lru_events.clone();
    let mut lru_ttl = LruTtlCache::<u32, u32>::builder()
        .max_size(10)
        .ttl(Duration::from_millis(20))
        .on_evict(move |k: &u32, v: &u32| sink.lock().unwrap().push((*k, *v)))
        .build()
        .unwrap();

    let sorted_outcome = retain_parity_case!(sorted, &sorted_events);
    let ttl_outcome = retain_parity_case!(ttl, &ttl_events);
    let lru_outcome = retain_parity_case!(lru_ttl, &lru_events);

    // Both live entries reach the predicate (the expired pair does not); key 3 survives;
    // three removals are counted and reported: the two expired plus the rejected key 4.
    let expected = (
        vec![3u32, 4],
        vec![3u32],
        3u64,
        vec![(1u32, 11u32), (2, 22), (4, 44)],
    );
    assert_eq!(sorted_outcome, expected, "TtlSortedCache::retain");
    assert_eq!(ttl_outcome, expected, "TtlCache::retain");
    assert_eq!(lru_outcome, expected, "LruTtlCache::retain");
}

// ===========================================================================
// Public-API coverage for the combined "size trim AND expiry sweep" path.
//
// `set_with(..).evict().set()` on a size-limited cache is the ONLY public route
// that reaches the trim/sweep combination (an over-capacity insert that also
// carries a cutoff). Everything else either trims with no cutoff (a plain `set`
// over the cap, `set_max_size`, `retain_latest(n, false)`) or sweeps with no trim
// (`evict()`, `retain_latest(n, true)` while under the cap).
// ===========================================================================

/// The expired prefix is larger than the size overflow: the insert must drop BOTH expired
/// entries (not just the single entry needed to get back under the cap), fire `on_evict` for
/// each in expiry order, and leave the cache below its cap.
#[test]
fn evicting_insert_over_the_cap_sweeps_the_whole_expired_prefix() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 3);
    cache
        .set_with(1u32, 11u32)
        .ttl(Duration::from_millis(30))
        .set();
    cache
        .set_with(2u32, 22u32)
        .ttl(Duration::from_millis(30))
        .set();
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(60))
        .set();
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_eq!(cache.cache_size(), 3, "at the cap, nothing swept yet");

    // Over the cap (4 > 3) with the sweep opted in: the trim needs 1 drop, the expiry sweep
    // wants 2, and the combined pass must take the union.
    cache
        .set_with(4u32, 44u32)
        .ttl(Duration::from_secs(60))
        .evict()
        .set();

    assert_eq!(sorted_keys(&cache), vec![3u32, 4]);
    assert_eq!(
        cache.cache_size(),
        2,
        "the sweep goes past the size overflow"
    );
    assert_eq!(
        fired(&events),
        vec![(1u32, 11u32), (2, 22)],
        "both expired entries are reported, in expiry order"
    );
    assert_eq!(cache.cache_evictions(), Some(2));
    // Nothing stale is left behind for a later sweep to double-count.
    assert_eq!(cache.evict(), 0);
    assert_eq!(cache.retain_latest(2, true), 0);
    assert_eq!(cache.cache_evictions(), Some(2));
}

/// The mirror case: the size overflow is larger than the expired prefix, so the trim must
/// continue into LIVE entries (soonest-to-expire first) and then stop at the first live
/// entry once the cap is satisfied.
#[test]
fn evicting_insert_over_the_cap_trims_past_the_expired_prefix_into_live_entries() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 2);
    cache
        .set_with(1u32, 11u32)
        .ttl(Duration::from_millis(30))
        .set();
    cache
        .set_with(2u32, 22u32)
        .ttl(Duration::from_secs(10))
        .set();
    // `set` (no `.evict()`) over the cap still trims, so add the third with the cap already
    // at 2: this drops the soonest-to-expire, which is the expired key 1.
    std::thread::sleep(std::time::Duration::from_millis(60));
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(20))
        .set();
    assert_eq!(sorted_keys(&cache), vec![2u32, 3]);
    assert_eq!(fired(&events), vec![(1u32, 11u32)]);

    // Now an evicting insert over the cap with NOTHING expired: the cutoff contributes
    // nothing and the trim alone picks the soonest-to-expire live entry.
    cache
        .set_with(4u32, 44u32)
        .ttl(Duration::from_secs(30))
        .evict()
        .set();
    assert_eq!(sorted_keys(&cache), vec![3u32, 4]);
    assert_eq!(
        fired(&events),
        vec![(1u32, 11u32), (2, 22)],
        "the live soonest-to-expire entry is the victim"
    );
    assert_eq!(cache.cache_evictions(), Some(2));
    assert_eq!(cache.cache_size(), 2);
}

/// Never-expiring entries must be retained last by the combined pass: they sort after every
/// finite expiry, so a trim only reaches them once every dated entry is gone.
#[test]
fn evicting_insert_over_the_cap_retains_never_expiring_entries_last() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 3);
    cache.set_with(1u32, 11u32).ttl(Duration::ZERO).set();
    cache.set_with(2u32, 22u32).ttl(Duration::ZERO).set();
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_millis(30))
        .set();
    std::thread::sleep(std::time::Duration::from_millis(60));

    // Over the cap with the sweep on: key 3 is expired AND is the only dated entry, so the
    // single required drop and the sweep pick the same victim; the two never-expiring
    // entries survive.
    cache
        .set_with(4u32, 44u32)
        .ttl(Duration::from_secs(60))
        .evict()
        .set();
    assert_eq!(sorted_keys(&cache), vec![1u32, 2, 4]);
    assert_eq!(fired(&events), vec![(3u32, 33u32)]);
    assert_eq!(cache.cache_evictions(), Some(1));

    // Push past the cap again with everything live: the dated key 4 must go before either
    // never-expiring entry.
    cache
        .set_with(5u32, 55u32)
        .ttl(Duration::from_secs(120))
        .evict()
        .set();
    assert_eq!(sorted_keys(&cache), vec![1u32, 2, 5]);
    assert_eq!(fired(&events), vec![(3u32, 33u32), (4, 44)]);

    // And only when no dated entry remains does a trim reach the never-expiring ones.
    assert_eq!(cache.retain_latest(1, true), 2);
    assert_eq!(sorted_keys(&cache), vec![2u32]);
}

/// An evicting insert that *replaces* an existing key while the cache is at its cap: the map
/// length never exceeds the cap, so no trim runs, but the sweep still must not be reachable
/// through this branch (the store only sweeps when it is over the cap). Pins the documented
/// asymmetry that `.evict()` on a size-limited cache is a no-op unless the insert overflows.
#[test]
fn evicting_insert_at_the_cap_without_overflow_does_not_sweep() {
    let (mut cache, events) = events_cache_capped(Duration::from_secs(60), 3);
    cache
        .set_with(1u32, 11u32)
        .ttl(Duration::from_millis(30))
        .set();
    cache
        .set_with(2u32, 22u32)
        .ttl(Duration::from_secs(60))
        .set();
    std::thread::sleep(std::time::Duration::from_millis(60));

    // Under the cap (2 -> 3) with `.evict()`: no overflow, so the expired key 1 stays.
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(60))
        .evict()
        .set();
    assert_eq!(
        cache.cache_size(),
        3,
        "a size-limited cache only sweeps when the insert overflows the cap"
    );
    assert_eq!(fired(&events), Vec::new());
    assert_eq!(cache.cache_evictions(), Some(0));

    // An explicit `evict()` still reaps it.
    assert_eq!(cache.evict(), 1);
    assert_eq!(fired(&events), vec![(1u32, 11u32)]);
    assert_eq!(cache.cache_evictions(), Some(1));
}

/// The same combined pass reached through `set_max_size` on a cache holding expired entries:
/// the shrink trims to the new bound (no cutoff), so expired entries are dropped only
/// because they sort first, and the resulting count must still match the observable state.
#[test]
fn set_max_size_shrink_drops_the_soonest_to_expire_first() {
    let (mut cache, events) = events_cache(Duration::from_secs(60));
    cache
        .set_with(1u32, 11u32)
        .ttl(Duration::from_millis(30))
        .set();
    cache
        .set_with(2u32, 22u32)
        .ttl(Duration::from_secs(10))
        .set();
    cache
        .set_with(3u32, 33u32)
        .ttl(Duration::from_secs(20))
        .set();
    cache.set_with(4u32, 44u32).ttl(Duration::ZERO).set();
    std::thread::sleep(std::time::Duration::from_millis(60));

    assert_eq!(cache.set_max_size(2), None, "no previous bound was set");
    assert_eq!(sorted_keys(&cache), vec![3u32, 4]);
    assert_eq!(
        fired(&events),
        vec![(1u32, 11u32), (2, 22)],
        "expiry order, expired first"
    );
    assert_eq!(cache.cache_evictions(), Some(2));
    assert_eq!(
        cache.evict(),
        0,
        "no stale index entries survive the shrink"
    );
}
