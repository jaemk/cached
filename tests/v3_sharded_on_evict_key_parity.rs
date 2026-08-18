//! Attaching an `on_evict` callback must not change which key a sharded store physically
//! stores on an overwrite.
//!
//! `ShardedTtlCache::cache_set` and `ShardedExpiringCache::cache_set` used to branch on
//! `self.inner.on_evict.is_some()`: the callback branch did `remove_entry` + `insert`
//! (replacing the stored key with the caller's) while the no-callback branch did a plain
//! `HashMap::insert` (keeping the stored key). Which key survived therefore depended on an
//! unrelated builder option:
//!
//! ```text
//! ttl  no-on_evict stored tag = "first"   with-on_evict stored tag = "second"
//! ```
//!
//! Both now take the single stored-key-keeping shape, so an `on_evict` callback is purely
//! observational.
//!
//! Each store's own overwrite semantics differ by backing store, and that difference is
//! pinned here too:
//!
//! * `HashMap`-backed (`ShardedUnboundCache`, `ShardedTtlCache`, `ShardedExpiringCache`) keep
//!   the **first-inserted** key, matching `HashMap::insert` and the single-owner `TtlCache` /
//!   `ExpiringCache`.
//! * `LruCache`-backed (`ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache`)
//!   rebind the slot to the **caller's** key, because the LRU primitive replaces the whole
//!   `(K, V)` slot (see `tests/v3_cert_expiring_lru_stored_key.rs`).
//!
//! Both are unconditional on `on_evict`, which is what this file certifies.

#![allow(clippy::redundant_closure_call)]

use cached::{
    ConcurrentCached, Expires, ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache,
    ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

use std::hash::{Hash, Hasher};

/// A key whose `Hash`/`Eq` cover only `id`, so two equal keys can still be told apart by `tag`.
#[derive(Clone, Debug)]
struct Tagged {
    id: u32,
    tag: &'static str,
}

impl PartialEq for Tagged {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Tagged {}
impl Hash for Tagged {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

fn k(id: u32, tag: &'static str) -> Tagged {
    Tagged { id, tag }
}

/// Sanity check on the instrument itself: without this every assertion below is vacuous.
#[test]
fn tagged_key_is_equal_but_distinguishable() {
    assert_eq!(k(1, "first"), k(1, "second"), "same id must compare equal");
    assert_ne!(k(1, "x").tag, k(1, "y").tag, "tags tell instances apart");
    assert_ne!(k(1, "x"), k(2, "x"), "different ids must not compare equal");
}

/// Value for the two `Expires`-driven stores. Never expires, so `cache_set` takes the plain
/// displaced-live-value path.
#[derive(Clone, Debug, PartialEq)]
struct Live(u32);

impl Expires for Live {
    fn is_expired(&self) -> bool {
        false
    }
}

/// Write `first` then `second` over the same id and report the `tag` of the key the store
/// actually kept, probed with a third key instance that is `Eq` to both.
fn overwrite_and_take_tag<C, V>(cache: &C, first: V, second: V) -> &'static str
where
    C: ConcurrentCached<Tagged, V>,
    V: Clone,
{
    let _ = ConcurrentCached::cache_set(cache, k(1, "first"), first);
    let _ = ConcurrentCached::cache_set(cache, k(1, "second"), second);
    let (stored, _) = ConcurrentCached::cache_remove_entry(cache, &k(1, "probe"))
        .ok()
        .flatten()
        .expect("the overwritten entry must still be present");
    stored.tag
}

/// Generate the parity test for one store.
///
/// `$make` is `|with_on_evict: bool| -> Store`; `$expected` is the tag of the key the store
/// keeps on an overwrite, which must hold either way.
macro_rules! on_evict_key_parity {
    ($name:ident, $first:expr, $second:expr, $expected:expr, $make:expr) => {
        #[test]
        fn $name() {
            let make = $make;
            let without = make(false);
            let with = make(true);
            let a = overwrite_and_take_tag(&without, $first, $second);
            let b = overwrite_and_take_tag(&with, $first, $second);
            assert_eq!(
                a, b,
                "an `on_evict` callback must not decide which key is stored"
            );
            assert_eq!(a, $expected, "without on_evict");
            assert_eq!(b, $expected, "with on_evict");
        }
    };
}

on_evict_key_parity!(
    sharded_unbound_keeps_the_first_key_either_way,
    10u32,
    20u32,
    "first",
    |with_on_evict: bool| {
        let b = ShardedUnboundCache::<Tagged, u32>::builder().shards(4);
        let b = if with_on_evict {
            b.on_evict(|_: &Tagged, _: &u32| {})
        } else {
            b
        };
        b.build().unwrap()
    }
);

#[cfg(feature = "time_stores")]
on_evict_key_parity!(
    sharded_ttl_keeps_the_first_key_either_way,
    10u32,
    20u32,
    "first",
    |with_on_evict: bool| {
        let b = ShardedTtlCache::<Tagged, u32>::builder()
            .shards(4)
            .ttl_secs(600);
        let b = if with_on_evict {
            b.on_evict(|_: &Tagged, _: &u32| {})
        } else {
            b
        };
        b.build().unwrap()
    }
);

on_evict_key_parity!(
    sharded_expiring_keeps_the_first_key_either_way,
    Live(10),
    Live(20),
    "first",
    |with_on_evict: bool| {
        let b = ShardedExpiringCache::<Tagged, Live>::builder().shards(4);
        let b = if with_on_evict {
            b.on_evict(|_: &Tagged, _: &Live| {})
        } else {
            b
        };
        b.build().unwrap()
    }
);

on_evict_key_parity!(
    sharded_lru_rebinds_to_the_caller_key_either_way,
    10u32,
    20u32,
    "second",
    |with_on_evict: bool| {
        let b = ShardedLruCache::<Tagged, u32>::builder()
            .shards(4)
            .max_size(64);
        let b = if with_on_evict {
            b.on_evict(|_: &Tagged, _: &u32| {})
        } else {
            b
        };
        b.build().unwrap()
    }
);

#[cfg(feature = "time_stores")]
on_evict_key_parity!(
    sharded_lru_ttl_rebinds_to_the_caller_key_either_way,
    10u32,
    20u32,
    "second",
    |with_on_evict: bool| {
        let b = ShardedLruTtlCache::<Tagged, u32>::builder()
            .shards(4)
            .max_size(64)
            .ttl_secs(600);
        // The typestate builder changes type on `.on_evict(..)`, so the two arms build
        // separately -- both yield the same `ShardedLruTtlCache<Tagged, u32>`.
        if with_on_evict {
            b.on_evict(|_: &Tagged, _: &u32| {}).build().unwrap()
        } else {
            b.build().unwrap()
        }
    }
);

on_evict_key_parity!(
    sharded_expiring_lru_rebinds_to_the_caller_key_either_way,
    Live(10),
    Live(20),
    "second",
    |with_on_evict: bool| {
        let b = ShardedExpiringLruCache::<Tagged, Live>::builder()
            .shards(4)
            .max_size(64);
        let b = if with_on_evict {
            b.on_evict(|_: &Tagged, _: &Live| {})
        } else {
            b
        };
        b.build().unwrap()
    }
);

/// Keeping the stored key must not change what `cache_set` reports: the displaced **live**
/// value is still returned. The `HashMap`-backed stores swap the value in place through
/// `get_mut` rather than going through `remove_entry` + `insert`, so this is the one thing
/// that could silently regress alongside the key.
#[test]
fn displaced_live_value_is_still_returned() {
    let unbound = ShardedUnboundCache::<Tagged, u32>::builder()
        .shards(1)
        .build()
        .unwrap();
    assert_eq!(
        ConcurrentCached::cache_set(&unbound, k(1, "first"), 10u32).unwrap(),
        None
    );
    assert_eq!(
        ConcurrentCached::cache_set(&unbound, k(1, "second"), 20u32).unwrap(),
        Some(10)
    );
    assert_eq!(
        ConcurrentCached::cache_get(&unbound, &k(1, "probe")).unwrap(),
        Some(20)
    );

    let expiring = ShardedExpiringCache::<Tagged, Live>::builder()
        .shards(1)
        .build()
        .unwrap();
    let _ = ConcurrentCached::cache_set(&expiring, k(1, "first"), Live(10));
    assert_eq!(
        ConcurrentCached::cache_set(&expiring, k(1, "second"), Live(20)).unwrap(),
        Some(Live(10))
    );

    #[cfg(feature = "time_stores")]
    {
        let ttl = ShardedTtlCache::<Tagged, u32>::builder()
            .shards(1)
            .ttl_secs(600)
            .build()
            .unwrap();
        let _ = ConcurrentCached::cache_set(&ttl, k(1, "first"), 10u32);
        assert_eq!(
            ConcurrentCached::cache_set(&ttl, k(1, "second"), 20u32).unwrap(),
            Some(10)
        );
    }
}

/// Overwriting an entry that has **already expired** filters the displaced value from the
/// return, fires `on_evict` once and counts one eviction -- from the same single write shape
/// that keeps the stored key. The callback receives the caller's key (the stored one stays in
/// the map, and the two compare `Eq`), after the shard lock is released.
#[test]
fn expired_displaced_entry_is_counted_and_notified() {
    use cached::ConcurrentCacheBase;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Always expired.
    #[derive(Clone, Debug, PartialEq)]
    struct Stale(u32);
    impl Expires for Stale {
        fn is_expired(&self) -> bool {
            true
        }
    }

    let fired = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&fired);
    let c = ShardedExpiringCache::<Tagged, Stale>::builder()
        .shards(1)
        .on_evict(move |_: &Tagged, _: &Stale| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();
    let _ = ConcurrentCached::cache_set(&c, k(1, "first"), Stale(10));
    assert_eq!(
        ConcurrentCached::cache_set(&c, k(1, "second"), Stale(20)).unwrap(),
        None,
        "an expired displaced value is filtered from the return"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "on_evict fires once for the displaced expired entry"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&c),
        Some(1),
        "one eviction counted"
    );
    // The overwrite still kept the stored key.
    let (stored, _) = ConcurrentCached::cache_remove_entry(&c, &k(1, "probe"))
        .unwrap()
        .expect("entry present");
    assert_eq!(stored.tag, "first");
}

/// The same accounting on the TTL store, whose displaced entry expires by wall clock.
#[cfg(feature = "time_stores")]
#[test]
fn expired_displaced_ttl_entry_is_counted_and_notified() {
    use cached::ConcurrentCacheBase;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let fired = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&fired);
    let c = ShardedTtlCache::<Tagged, u32>::builder()
        .shards(1)
        .ttl_millis(20)
        .on_evict(move |_: &Tagged, _: &u32| {
            sink.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();
    let _ = ConcurrentCached::cache_set(&c, k(1, "first"), 10u32);
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert_eq!(
        ConcurrentCached::cache_set(&c, k(1, "second"), 20u32).unwrap(),
        None,
        "an expired displaced value is filtered from the return"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "on_evict fires once for the displaced expired entry"
    );
    assert_eq!(
        ConcurrentCacheBase::cache_evictions(&c),
        Some(1),
        "one eviction counted"
    );
    let (stored, _) = ConcurrentCached::cache_remove_entry(&c, &k(1, "probe"))
        .unwrap()
        .expect("entry present");
    assert_eq!(stored.tag, "first");
}
