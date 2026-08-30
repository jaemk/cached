//! A hand-written, non-`BuildHasher` [`cached::ShardHasher`] router driving the **trait** surface
//! of the six sharded stores: `ConcurrentCached`, `ConcurrentCachedExt`, `ConcurrentCachePeek`,
//! and (under `async_core`) `ConcurrentCachedAsync` / `ConcurrentCachedAsyncExt` /
//! `ConcurrentCachePeekAsync`.
//!
//! The crate root documents this explicitly -- "The trait forms remain available for owned keys on
//! any hasher: `ConcurrentCachedExt::get(&cache, &key).unwrap()` ... plus
//! `ConcurrentCachePeek::peek(&cache, &key).unwrap()`" -- but no test drove the *full* trait
//! surface (peek, remove_entry, and the async forms) over a store carrying a hand-written,
//! non-`BuildHasher` router, so nothing checked that such a router satisfies the traits'
//! `H: ShardHasher<K>` bounds at all, let alone that it routes to the same shard the inherent
//! methods do.
//!
//! Every call below goes through a *generic* helper bounded on the trait, never on the concrete
//! store type. That is deliberate: an inherent method wins at a concrete call site, so
//! `cache.get(&k)` would silently exercise the inherent lookup instead of the trait one. Routing
//! through a generic function is the only way to be sure the trait impl is what ran.
//!
//! The traits take owned keys only (`&K`, not `&Q`), so a single `impl ShardHasher<K>` is the
//! whole requirement -- `OwnedOnlyRouter` here carries exactly one impl. The borrowed-key surface
//! is the inherent methods' business and is covered in
//! `tests/sharded_router_agreement_per_store.rs`.

use std::fmt::Debug;
use std::hash::{Hash, Hasher};

use cached::{
    ConcurrentCachePeek, ConcurrentCached, ConcurrentCachedExt, Expires, ShardHasher,
    ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache, ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// 2^64 / phi, so the upper 32 bits (the half shard selection reads) are well spread.
const PHI: u64 = 0x9e37_79b9_7f4a_7c15;

const SHARDS: usize = 8;
const KEYS: u64 = 32;
/// `MAX_SIZE / SHARDS` = 64 >= `KEYS`, so no bounded store can evict during these tests.
const MAX_SIZE: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
struct UserId(u64);

impl Hash for UserId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Deliberately not a `BuildHasher`, and carrying exactly one `ShardHasher` impl: the owned key
/// type the trait surface takes.
#[derive(Clone)]
struct OwnedOnlyRouter;

impl ShardHasher<UserId> for OwnedOnlyRouter {
    fn shard_hash(&self, key: &UserId) -> u64 {
        key.0.wrapping_mul(PHI)
    }
}

/// Value type for the two expiring stores. Never expires: this file is about routing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Live(u64);

impl Expires for Live {
    fn is_expired(&self) -> bool {
        false
    }
}

// -------------------------------------------------------------------------------------------
// Generic, trait-bounded helpers. Nothing below names a concrete store type, so an inherent
// method cannot shadow the trait method being exercised.
// -------------------------------------------------------------------------------------------

fn set_via_ext<K, V, T>(c: &T, k: K, v: V) -> Option<V>
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::set(c, k, v).expect("sharded stores are infallible")
}

fn get_via_ext<K, V, T>(c: &T, k: &K) -> Option<V>
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::get(c, k).expect("sharded stores are infallible")
}

fn contains_via_ext<K, V, T>(c: &T, k: &K) -> bool
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::contains(c, k).expect("sharded stores are infallible")
}

fn remove_via_ext<K, V, T>(c: &T, k: &K) -> Option<V>
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::remove(c, k).expect("sharded stores are infallible")
}

fn remove_entry_via_ext<K, V, T>(c: &T, k: &K) -> Option<(K, V)>
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::remove_entry(c, k).expect("sharded stores are infallible")
}

fn delete_via_ext<K, V, T>(c: &T, k: &K) -> bool
where
    T: ConcurrentCachedExt<K, V>,
    T::Error: Debug,
{
    ConcurrentCachedExt::delete(c, k).expect("sharded stores are infallible")
}

fn peek_via_trait<K, V, T>(c: &T, k: &K) -> Option<V>
where
    T: ConcurrentCachePeek<K, V>,
    T::Error: Debug,
{
    ConcurrentCachePeek::peek(c, k).expect("sharded stores are infallible")
}

/// The base trait's own `cache_get`, distinct from the `ConcurrentCachedExt::get` alias above.
fn cache_get_via_trait<K, V, T>(c: &T, k: &K) -> Option<V>
where
    T: ConcurrentCached<K, V>,
    T::Error: Debug,
{
    ConcurrentCached::cache_get(c, k).expect("sharded stores are infallible")
}

/// The whole owned-key trait script, run against one store. Written once and applied to all six
/// so no store can quietly drop off the list.
fn exercise_trait_surface<V, T>(c: &T, val: impl Fn(u64) -> V, store: &str)
where
    V: Clone + PartialEq + Debug,
    T: ConcurrentCachedExt<UserId, V> + ConcurrentCachePeek<UserId, V>,
    T::Error: Debug,
{
    for id in 0..KEYS {
        assert_eq!(
            set_via_ext(c, UserId(id), val(id)),
            None,
            "{store}: a fresh `set` through the trait must displace nothing"
        );
    }

    for id in 0..KEYS {
        assert_eq!(
            get_via_ext(c, &UserId(id)),
            Some(val(id)),
            "{store}: trait `get` missed `UserId({id})` under a hand-written router"
        );
        assert_eq!(
            cache_get_via_trait(c, &UserId(id)),
            Some(val(id)),
            "{store}: `cache_get` missed `UserId({id})` under a hand-written router"
        );
        assert_eq!(
            peek_via_trait(c, &UserId(id)),
            Some(val(id)),
            "{store}: trait `peek` missed `UserId({id})` under a hand-written router"
        );
        assert!(
            contains_via_ext(c, &UserId(id)),
            "{store}: trait `contains` missed `UserId({id})` under a hand-written router"
        );
    }

    let absent = UserId(KEYS + 1);
    assert_eq!(get_via_ext(c, &absent), None, "{store}: absent trait `get`");
    assert_eq!(
        peek_via_trait(c, &absent),
        None,
        "{store}: absent trait `peek`"
    );
    assert!(
        !contains_via_ext(c, &absent),
        "{store}: absent trait `contains`"
    );

    // Each removing method gets its own third of the key space.
    for id in 0..KEYS {
        match id % 3 {
            0 => assert_eq!(
                remove_via_ext(c, &UserId(id)),
                Some(val(id)),
                "{store}: trait `remove` missed `UserId({id})`"
            ),
            1 => assert_eq!(
                remove_entry_via_ext(c, &UserId(id)),
                Some((UserId(id), val(id))),
                "{store}: trait `remove_entry` must hand back the stored owned key for `UserId({id})`"
            ),
            _ => assert!(
                delete_via_ext(c, &UserId(id)),
                "{store}: trait `delete` missed `UserId({id})`"
            ),
        }
        assert!(
            !contains_via_ext(c, &UserId(id)),
            "{store}: `UserId({id})` must be gone after its removal"
        );
    }
}

/// Generates, per store, the trait-surface script and the inherent/trait cross-check.
macro_rules! per_store_trait_surface_tests {
    ($name:ident, $label:literal, $build:expr, $val:expr) => {
        mod $name {
            use super::*;

            /// Every owned-key trait form works on a store built over a hand-written router, and
            /// finds the entries the same router placed.
            #[test]
            fn the_owned_key_trait_forms_work_over_a_hand_written_router() {
                let c = $build;
                exercise_trait_surface(&c, $val, $label);
            }

            /// The inherent methods and the trait methods must reach the same entry. They take
            /// different internal routes for the same owned key -- `cache_set` / `cache_get` go
            /// through the store's `shard_of`, while the inherent `get` goes through
            /// `shard_of_borrowed` at `Q = K` -- so a router whose two routes diverged would show
            /// up here as a miss even though both bounds resolve to the same `ShardHasher<K>`
            /// impl.
            #[test]
            fn the_inherent_and_trait_forms_reach_the_same_entry() {
                let val = $val;
                let c = $build;

                // Written through the trait, read through the inherent method.
                for id in 0..KEYS {
                    assert_eq!(set_via_ext(&c, UserId(id), val(id)), None);
                }
                for id in 0..KEYS {
                    assert_eq!(
                        c.get(&UserId(id)),
                        Some(val(id)),
                        concat!(
                            $label,
                            ": the inherent `get` must find what the trait `set` stored for `UserId({})`"
                        ),
                        id
                    );
                }

                // And the other direction: written through the inherent method, read through the
                // trait.
                let c = $build;
                for id in 0..KEYS {
                    c.set(UserId(id), val(id));
                }
                for id in 0..KEYS {
                    assert_eq!(
                        get_via_ext(&c, &UserId(id)),
                        Some(val(id)),
                        concat!(
                            $label,
                            ": the trait `get` must find what the inherent `set` stored for `UserId({})`"
                        ),
                        id
                    );
                    assert_eq!(
                        peek_via_trait(&c, &UserId(id)),
                        c.peek(&UserId(id)),
                        concat!($label, ": trait and inherent `peek` must agree for `UserId({})`"),
                        id
                    );
                }
            }
        }
    };
}

per_store_trait_surface_tests!(
    sharded_unbound,
    "ShardedUnboundCache",
    ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap(),
    |id: u64| id * 10
);

per_store_trait_surface_tests!(
    sharded_lru,
    "ShardedLruCache",
    ShardedLruCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap(),
    |id: u64| id * 10
);

per_store_trait_surface_tests!(
    sharded_expiring,
    "ShardedExpiringCache",
    ShardedExpiringCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap(),
    |id: u64| Live(id * 10)
);

per_store_trait_surface_tests!(
    sharded_expiring_lru,
    "ShardedExpiringLruCache",
    ShardedExpiringLruCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap(),
    |id: u64| Live(id * 10)
);

#[cfg(feature = "time_stores")]
mod time_stores {
    use super::*;

    /// One hour: nothing under test may expire mid-run.
    const TTL: std::time::Duration = std::time::Duration::from_secs(3600);

    per_store_trait_surface_tests!(
        sharded_ttl,
        "ShardedTtlCache",
        ShardedTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .ttl(TTL)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );

    per_store_trait_surface_tests!(
        sharded_lru_ttl,
        "ShardedLruTtlCache",
        ShardedLruTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .max_size(MAX_SIZE)
            .ttl(TTL)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );
}

/// The async trait surface over the same hand-written router. `async_core` defines the traits;
/// the sharded stores never block, so their async methods delegate to the sync ones -- but the
/// delegation still has to satisfy `H: ShardHasher<K>`, which no test covered for a router. Driven
/// over all six stores by macro, the same way the sync half above is, so no store can quietly
/// drop off the async list either.
#[cfg(feature = "async_core")]
mod async_surface {
    use super::*;
    use cached::{ConcurrentCachePeekAsync, ConcurrentCachedAsync, ConcurrentCachedAsyncExt};

    async fn async_set_via_ext<K, V, T>(c: &T, k: K, v: V) -> Option<V>
    where
        T: ConcurrentCachedAsyncExt<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_set(c, k, v)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_get_via_ext<K, V, T>(c: &T, k: &K) -> Option<V>
    where
        T: ConcurrentCachedAsyncExt<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_get(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_contains_via_ext<K, V, T>(c: &T, k: &K) -> bool
    where
        T: ConcurrentCachedAsyncExt<K, V> + Sync,
        K: Sync,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_contains(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_cache_get_via_trait<K, V, T>(c: &T, k: &K) -> Option<V>
    where
        T: ConcurrentCachedAsync<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachedAsync::async_cache_get(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_peek_via_trait<K, V, T>(c: &T, k: &K) -> Option<V>
    where
        T: ConcurrentCachePeekAsync<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachePeekAsync::async_cache_peek(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_remove_via_ext<K, V, T>(c: &T, k: &K) -> Option<V>
    where
        T: ConcurrentCachedAsyncExt<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_remove(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_remove_entry_via_ext<K, V, T>(c: &T, k: &K) -> Option<(K, V)>
    where
        T: ConcurrentCachedAsyncExt<K, V>,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_remove_entry(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    async fn async_delete_via_ext<K, V, T>(c: &T, k: &K) -> bool
    where
        T: ConcurrentCachedAsyncExt<K, V> + Sync,
        K: Sync,
        T::Error: Debug,
    {
        ConcurrentCachedAsyncExt::async_delete(c, k)
            .await
            .expect("sharded stores are infallible")
    }

    /// The whole async trait script, run against one store. Written once and applied to all six
    /// so no store can quietly drop off the list, and covers every async trait method this file
    /// claims to exercise: `async_get`, `async_cache_get`, `async_cache_peek`, `async_contains`,
    /// `async_remove`, `async_remove_entry`, and `async_delete`.
    async fn exercise_async_surface<V, T>(c: &T, val: impl Fn(u64) -> V, store: &str)
    where
        V: Clone + PartialEq + Debug + Send + Sync,
        T: ConcurrentCachedAsyncExt<UserId, V> + ConcurrentCachePeekAsync<UserId, V> + Sync,
        T::Error: Debug,
    {
        for id in 0..KEYS {
            assert_eq!(async_set_via_ext(c, UserId(id), val(id)).await, None);
        }
        for id in 0..KEYS {
            assert_eq!(
                async_get_via_ext(c, &UserId(id)).await,
                Some(val(id)),
                "{store}: async `get` missed `UserId({id})` under a hand-written router"
            );
            assert_eq!(
                async_cache_get_via_trait(c, &UserId(id)).await,
                Some(val(id)),
                "{store}: `async_cache_get` missed `UserId({id})` under a hand-written router"
            );
            assert_eq!(
                async_peek_via_trait(c, &UserId(id)).await,
                Some(val(id)),
                "{store}: `async_cache_peek` missed `UserId({id})` under a hand-written router"
            );
            assert!(
                async_contains_via_ext(c, &UserId(id)).await,
                "{store}: async `contains` missed `UserId({id})` under a hand-written router"
            );
        }

        let absent = UserId(KEYS + 1);
        assert_eq!(
            async_get_via_ext(c, &absent).await,
            None,
            "{store}: an absent key must miss on the async surface too"
        );
        assert_eq!(
            async_peek_via_trait(c, &absent).await,
            None,
            "{store}: absent async `peek`"
        );
        assert!(
            !async_contains_via_ext(c, &absent).await,
            "{store}: absent async `contains`"
        );

        // Each removing method gets its own third of the key space, mirroring the sync script.
        for id in 0..KEYS {
            match id % 3 {
                0 => assert_eq!(
                    async_remove_via_ext(c, &UserId(id)).await,
                    Some(val(id)),
                    "{store}: async `remove` missed `UserId({id})`"
                ),
                1 => assert_eq!(
                    async_remove_entry_via_ext(c, &UserId(id)).await,
                    Some((UserId(id), val(id))),
                    "{store}: async `remove_entry` must hand back the stored owned key for `UserId({id})`"
                ),
                _ => assert!(
                    async_delete_via_ext(c, &UserId(id)).await,
                    "{store}: async `delete` missed `UserId({id})`"
                ),
            }
            assert!(
                !async_contains_via_ext(c, &UserId(id)).await,
                "{store}: `UserId({id})` must be gone after its async removal"
            );
        }
    }

    /// Generates, per store, one `#[tokio::test]` running the async trait-surface script.
    macro_rules! per_store_async_surface_tests {
        ($name:ident, $label:literal, $build:expr, $val:expr) => {
            mod $name {
                use super::*;

                #[tokio::test]
                async fn the_async_trait_forms_work_over_a_hand_written_router() {
                    let c = $build;
                    exercise_async_surface(&c, $val, $label).await;
                }
            }
        };
    }

    per_store_async_surface_tests!(
        sharded_unbound,
        "ShardedUnboundCache",
        ShardedUnboundCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );

    per_store_async_surface_tests!(
        sharded_lru,
        "ShardedLruCache",
        ShardedLruCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .max_size(MAX_SIZE)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );

    per_store_async_surface_tests!(
        sharded_expiring,
        "ShardedExpiringCache",
        ShardedExpiringCache::<UserId, Live>::builder()
            .shards(SHARDS)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| Live(id * 10)
    );

    per_store_async_surface_tests!(
        sharded_expiring_lru,
        "ShardedExpiringLruCache",
        ShardedExpiringLruCache::<UserId, Live>::builder()
            .shards(SHARDS)
            .max_size(MAX_SIZE)
            .hasher(OwnedOnlyRouter)
            .build()
            .unwrap(),
        |id: u64| Live(id * 10)
    );

    #[cfg(feature = "time_stores")]
    mod time_stores {
        use super::*;

        /// One hour: nothing under test may expire mid-run.
        const TTL: std::time::Duration = std::time::Duration::from_secs(3600);

        per_store_async_surface_tests!(
            sharded_ttl,
            "ShardedTtlCache",
            ShardedTtlCache::<UserId, u64>::builder()
                .shards(SHARDS)
                .ttl(TTL)
                .hasher(OwnedOnlyRouter)
                .build()
                .unwrap(),
            |id: u64| id * 10
        );

        per_store_async_surface_tests!(
            sharded_lru_ttl,
            "ShardedLruTtlCache",
            ShardedLruTtlCache::<UserId, u64>::builder()
                .shards(SHARDS)
                .max_size(MAX_SIZE)
                .ttl(TTL)
                .hasher(OwnedOnlyRouter)
                .build()
                .unwrap(),
            |id: u64| id * 10
        );
    }
}
