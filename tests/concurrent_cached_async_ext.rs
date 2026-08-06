/*!
`ConcurrentCachedAsyncExt`: the deduplicated short aliases over `ConcurrentCachedAsync`.

Every alias is exercised against a sharded store and checked to agree with the
`async_cache_`-prefixed method it delegates to. The aliases keep the `async_` prefix rather than
being bare `get`/`set`, because the stores that implement `ConcurrentCachedAsync` also implement
the synchronous `ConcurrentCached`, whose own alias trait `ConcurrentCachedExt` is blanket
implemented and lives in the same prelude; a bare name would be a second applicable candidate at
the call site. The final tests pin that both alias traits coexist -- through the prelude glob and
through a generic bound naming both -- which is the property the prefix buys.

The trait aliases only genuinely asynchronous operations. Size and metric introspection stays on
`ConcurrentCacheBase` (`cache_size`, `cache_is_empty`, `cache_hits`, ...), which is callable on
an async store with no extension trait imported, so there is nothing here to alias.

No Redis server required.
*/

#![cfg(feature = "async_core")]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::{ConcurrentCachedAsync, ConcurrentCachedAsyncExt, ShardedUnboundCache};
use futures::executor::block_on;

fn store() -> ShardedUnboundCache<String, u32> {
    ShardedUnboundCache::builder()
        .shards(2)
        .build()
        .expect("build ShardedUnboundCache")
}

fn key(k: &str) -> String {
    k.to_string()
}

/// `async_set` returns the displaced value and `async_get` reads back what was written; both
/// agree with the `async_cache_`-prefixed methods they delegate to.
#[test]
fn async_get_and_async_set_round_trip() {
    let cache = store();
    block_on(async {
        assert_eq!(cache.async_set(key("a"), 1).await.unwrap(), None);
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), Some(1));

        // The alias reports the previous value on overwrite, exactly like async_cache_set.
        assert_eq!(cache.async_set(key("a"), 2).await.unwrap(), Some(1));
        assert_eq!(cache.async_cache_get(&key("a")).await.unwrap(), Some(2));

        // A miss is None, not an error.
        assert_eq!(cache.async_get(&key("absent")).await.unwrap(), None);
    });
}

/// `async_remove` returns the removed value; `async_remove_entry` returns the stored key with it.
#[test]
fn async_remove_and_async_remove_entry_return_the_displaced_entry() {
    let cache = store();
    block_on(async {
        cache.async_set(key("a"), 1).await.unwrap();
        assert_eq!(cache.async_remove(&key("a")).await.unwrap(), Some(1));
        assert_eq!(cache.async_remove(&key("a")).await.unwrap(), None);

        cache.async_set(key("b"), 2).await.unwrap();
        assert_eq!(
            cache.async_remove_entry(&key("b")).await.unwrap(),
            Some((key("b"), 2))
        );
        assert_eq!(cache.async_remove_entry(&key("b")).await.unwrap(), None);
    });
}

/// `async_delete` reports whether an entry was physically removed, without returning the value.
#[test]
fn async_delete_reports_whether_an_entry_was_removed() {
    let cache = store();
    block_on(async {
        cache.async_set(key("a"), 1).await.unwrap();
        assert!(cache.async_delete(&key("a")).await.unwrap());
        assert!(!cache.async_delete(&key("a")).await.unwrap());
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), None);
    });
}

/// `async_contains` reports presence and agrees with `async_cache_contains`.
#[test]
fn async_contains_reports_presence() {
    let cache = store();
    block_on(async {
        assert!(!cache.async_contains(&key("a")).await.unwrap());
        cache.async_set(key("a"), 1).await.unwrap();
        assert!(cache.async_contains(&key("a")).await.unwrap());
        assert_eq!(
            cache.async_contains(&key("a")).await.unwrap(),
            cache.async_cache_contains(&key("a")).await.unwrap()
        );
    });
}

/// `async_clear` empties the store while preserving metrics; `async_reset` zeroes the metrics
/// too. The distinction is the reason both aliases exist. Metrics are read through
/// `ConcurrentCacheBase`, which the trait deliberately does not alias.
#[test]
fn async_clear_preserves_metrics_and_async_reset_zeroes_them() {
    use cached::ConcurrentCacheBase;

    let cache = store();
    block_on(async {
        cache.async_set(key("a"), 1).await.unwrap();
        cache.async_get(&key("a")).await.unwrap(); // one hit
        cache.async_get(&key("zzz")).await.unwrap(); // one miss

        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));

        cache.async_clear().await.unwrap();
        assert_eq!(cache.cache_size().unwrap(), Some(0));
        assert_eq!(
            (cache.cache_hits(), cache.cache_misses()),
            (Some(1), Some(1)),
            "async_clear must keep the counters"
        );

        cache.async_set(key("b"), 2).await.unwrap();
        cache.async_reset().await.unwrap();
        assert_eq!(cache.cache_size().unwrap(), Some(0));
        assert_eq!(
            (cache.cache_hits(), cache.cache_misses()),
            (Some(0), Some(0)),
            "async_reset must zero the counters"
        );
    });
}

/// `async_get_or_set_with` runs the initializer exactly once for a key: the first call is a miss
/// that awaits the closure and stores the result, the second is a hit served from the store.
/// Counting closure invocations proves the alias reaches the store rather than recomputing.
#[test]
fn async_get_or_set_with_runs_the_initializer_only_on_a_miss() {
    use cached::ConcurrentCacheBase;

    let cache = store();
    let calls = AtomicUsize::new(0);

    block_on(async {
        let first = cache
            .async_get_or_set_with(key("a"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                42
            })
            .await
            .unwrap();
        assert_eq!(first, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // The value really landed in the store, not just in the return value.
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), Some(42));
        assert_eq!(cache.cache_size().unwrap(), Some(1));

        let second = cache
            .async_get_or_set_with(key("a"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                99
            })
            .await
            .unwrap();
        assert_eq!(second, 42, "the hit must return the stored value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the initializer must not run on a hit"
        );

        // Same observable behaviour as the method it delegates to.
        let via_core = cache
            .async_cache_get_or_set_with(key("a"), || async { 7 })
            .await
            .unwrap();
        assert_eq!(via_core, 42);
    });
}

/// `async_try_get_or_set_with` keeps the two error channels separate: an `Err` from the closure
/// arrives in the inner `Result` and stores nothing, while a later `Ok` is cached and then
/// served on the next call without re-running the closure.
#[test]
fn async_try_get_or_set_with_stores_only_on_success() {
    use cached::ConcurrentCacheBase;

    let cache = store();
    let calls = AtomicUsize::new(0);

    block_on(async {
        let failed: Result<u32, &str> = cache
            .async_try_get_or_set_with(key("a"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("boom")
            })
            .await
            .expect("the store itself must not error");
        assert_eq!(failed, Err("boom"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.cache_size().unwrap(),
            Some(0),
            "an Err initializer must store nothing"
        );
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), None);

        let ok: Result<u32, &str> = cache
            .async_try_get_or_set_with(key("a"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            })
            .await
            .unwrap();
        assert_eq!(ok, Ok(42));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), Some(42));

        let cached_hit: Result<u32, &str> = cache
            .async_try_get_or_set_with(key("a"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(99)
            })
            .await
            .unwrap();
        assert_eq!(cached_hit, Ok(42));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the initializer must not run on a hit"
        );
    });
}

/// Every alias is reachable from `cached::prelude::*` with no per-trait import, on a store that
/// also implements the synchronous `ConcurrentCached` (and therefore also picks up
/// `ConcurrentCachedExt` from the same glob). If the async aliases were named bare `get`/`set`
/// this module would not compile.
#[test]
fn the_aliases_resolve_through_the_prelude_glob_alongside_the_sync_aliases() {
    use cached::prelude::*;

    let cache = store();
    block_on(async {
        // Async aliases (ConcurrentCachedAsyncExt).
        assert_eq!(cache.async_set(key("a"), 1).await.unwrap(), None);
        assert_eq!(cache.async_get(&key("a")).await.unwrap(), Some(1));

        // Sync aliases on the same store, in the same scope (ConcurrentCachedExt), reached
        // through fully-qualified syntax because the concrete type's inherent `get`/`set` take
        // call-site priority.
        assert_eq!(
            ConcurrentCachedExt::get(&cache, &key("a")).unwrap(),
            Some(1)
        );
        assert_eq!(ConcurrentCachedExt::set(&cache, key("b"), 2).unwrap(), None);

        assert_eq!(cache.async_remove(&key("b")).await.unwrap(), Some(2));
    });
}

/// A generic bound naming both alias traits at once resolves every method unambiguously.
/// Inherent methods do not apply in generic code, so this is the strictest form of the
/// no-collision property.
#[test]
fn both_alias_traits_can_be_named_in_one_generic_bound() {
    async fn exercise<C>(cache: &C)
    where
        C: cached::ConcurrentCachedExt<String, u32> + ConcurrentCachedAsyncExt<String, u32> + Sync,
    {
        // Sync alias, then async alias, on the same generic value.
        assert_eq!(cache.set(key("g"), 7).unwrap(), None);
        assert_eq!(cache.async_get(&key("g")).await.unwrap(), Some(7));
        assert_eq!(cache.async_remove(&key("g")).await.unwrap(), Some(7));
        assert!(!cache.contains(&key("g")).unwrap());
        assert!(!cache.async_contains(&key("g")).await.unwrap());

        // The get-or-set pair is unambiguous under both bounds too, in both spellings.
        assert_eq!(cache.get_or_set_with(key("g"), || 1).unwrap(), 1);
        assert_eq!(
            cache
                .async_get_or_set_with(key("g"), || async { 2 })
                .await
                .unwrap(),
            1,
            "the sync alias already stored the value"
        );
        let inner: Result<u32, ()> = cache
            .async_try_get_or_set_with(key("h"), || async { Ok(3) })
            .await
            .unwrap();
        assert_eq!(inner, Ok(3));
    }

    let cache = store();
    block_on(exercise(&cache));
}
