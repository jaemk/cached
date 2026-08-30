//! The cross-impl consistency contract on [`cached::ShardHasher`], exercised on **all six**
//! sharded stores rather than on one of them.
//!
//! `tests/sharded_custom_router_lookups.rs` covers break class 1 (a hand-written router keeps its
//! inherent owned lookups) per store, and the borrowed half of the contract on
//! `ShardedUnboundCache` only. The borrowed half is the half that depends on each store's own
//! private `shard_of_borrowed<Q>` helper, and there are six separately written copies of that
//! helper. This file holds every one of them to the same two properties:
//!
//! 1. an *agreeing* two-impl router makes every owned insert reachable through all six borrowed
//!    lookups -- which also proves the borrowed path routes through the store's `H` rather than
//!    through some other hasher, since the owned inserts were placed by `H`'s own `shard_hash`;
//! 2. a *disagreeing* two-impl router loses every present entry through all six borrowed forms,
//!    while the owned form keeps finding them and `len()` never moves.
//!
//! Property 2 is the documented footgun, pinned as a hazard rather than endorsed: a real router
//! must satisfy `shard_hash(&k) == shard_hash(k.borrow())`, as `AgreeingRouter` below does.
//!
//! ## Why the two forms are expected to land on different shards
//!
//! `DisagreeingRouter`'s borrowed impl is its owned impl at the bit-flipped key
//! (`borrowed(x) == owned(x ^ 1)`, pinned by
//! `the_disagreeing_routers_borrowed_impl_equals_its_owned_impl_at_the_flipped_key`), so a
//! borrowed lookup of `id` is *expected* to consult the shard an owned insert of
//! `UserId(id ^ 1)` lands in -- both read back from `shard_sizes()`, a public accessor. That
//! expectation is derived from the router identity above, not observed on the borrowed lookup
//! path itself, so `a_disagreeing_routers_two_forms_consult_different_shards` documents the
//! reasoning rather than pinning behavior on its own. The actual behavioral pin -- that the
//! borrowed path really does miss a present entry -- is
//! `a_disagreeing_router_loses_present_entries_through_every_borrowed_lookup`, which calls the
//! borrowed lookups directly on each store.

use std::borrow::Borrow;
use std::hash::{Hash, Hasher};

use cached::{
    Expires, ShardHasher, ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache,
    ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// 2^64 / phi. Spreads entropy into the upper 32 bits, the half shard selection reads.
const PHI: u64 = 0x9e37_79b9_7f4a_7c15;

const SHARDS: usize = 8;
const KEYS: u64 = 64;
/// Per-shard capacity works out to `MAX_SIZE / SHARDS` = 64 >= `KEYS`, so no bounded store can
/// evict during these tests even if every key routed to one shard. A capacity eviction would
/// otherwise be indistinguishable from a routing miss.
const MAX_SIZE: usize = 512;
/// Key pairs `(id, id ^ 1)` used by the shard-divergence check. Kept small because it rebuilds a
/// store per observation.
const SAMPLE: u64 = 8;

/// A newtype over a primitive, borrowable as that primitive, hashing as that primitive (the
/// `Borrow` contract). Only the router decides whether the owned and borrowed forms meet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UserId(u64);

impl Hash for UserId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Borrow<u64> for UserId {
    fn borrow(&self) -> &u64 {
        &self.0
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

/// A router with one impl per key type it routes, the two agreeing on keys that compare equal.
/// This is what the `ShardHasher` rustdoc requires of a multi-impl router.
#[derive(Clone)]
struct AgreeingRouter;

impl ShardHasher<UserId> for AgreeingRouter {
    fn shard_hash(&self, key: &UserId) -> u64 {
        key.0.wrapping_mul(PHI)
    }
}

impl ShardHasher<u64> for AgreeingRouter {
    fn shard_hash(&self, key: &u64) -> u64 {
        key.wrapping_mul(PHI)
    }
}

/// A router whose two impls deliberately VIOLATE the consistency contract, written to pin the
/// documented consequence -- not as a pattern to copy.
///
/// Each impl places its routing bits directly in the upper half (`x << 32`), and the borrowed impl
/// flips bit 0 of the key first. The flip is what makes the divergence deterministic on any shard
/// count >= 2 and on any run, and it is also what makes the borrowed form's destination
/// observable: `borrowed(id)` is literally `owned(id ^ 1)`.
#[derive(Clone)]
struct DisagreeingRouter;

impl ShardHasher<UserId> for DisagreeingRouter {
    fn shard_hash(&self, key: &UserId) -> u64 {
        key.0 << 32
    }
}

impl ShardHasher<u64> for DisagreeingRouter {
    fn shard_hash(&self, key: &u64) -> u64 {
        (key ^ 1) << 32
    }
}

/// The index of the one occupied shard of a freshly built store holding exactly one entry.
///
/// Read from the public `shard_sizes()`, so it reports where the store's own router actually put
/// the key. Nothing here reimplements the shard-index formula.
fn sole_occupied_shard(sizes: &[usize], what: &str) -> usize {
    let occupied: Vec<usize> = sizes
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        occupied.len(),
        1,
        "{what}: a store holding one entry must occupy exactly one shard, saw {sizes:?}"
    );
    occupied[0]
}

/// The identity the divergence check rests on: `DisagreeingRouter`'s borrowed impl is its owned
/// impl evaluated at the bit-flipped key, and never at the key itself. Stated at the router level,
/// where it is a property of the fixture rather than of any store.
#[test]
fn the_disagreeing_routers_borrowed_impl_equals_its_owned_impl_at_the_flipped_key() {
    let router = DisagreeingRouter;
    for id in 0..KEYS {
        assert_eq!(
            ShardHasher::<u64>::shard_hash(&router, &id),
            ShardHasher::<UserId>::shard_hash(&router, &UserId(id ^ 1)),
            "the borrowed impl must equal the owned impl at `UserId({} ^ 1)`",
            id
        );
        assert_ne!(
            ShardHasher::<u64>::shard_hash(&router, &id),
            ShardHasher::<UserId>::shard_hash(&router, &UserId(id)),
            "the two impls must disagree at `UserId({id})` for this fixture to mean anything"
        );
    }
}

/// The agreeing router satisfies the contract it claims to: equal keys hash equally through both
/// impls. Asserted at the router level so a store-level miss below cannot be blamed on the
/// fixture.
#[test]
fn the_agreeing_router_satisfies_the_cross_impl_consistency_contract() {
    let router = AgreeingRouter;
    for raw in [0u64, 1, 7, 63, 0x9e37_79b9, u64::MAX] {
        let owned = UserId(raw);
        let borrowed: &u64 = owned.borrow();
        assert_eq!(
            ShardHasher::<UserId>::shard_hash(&router, &owned),
            ShardHasher::<u64>::shard_hash(&router, borrowed),
            "shard_hash disagrees between UserId({raw}) and its borrowed u64"
        );
    }
}

/// The shard-count boundary the footgun depends on. With `shards(1)` the mask is `0`, so
/// `shard_index` returns shard 0 for every hash and the two disagreeing impls cannot send a key
/// anywhere different: the same contract-violating router that loses every entry above loses
/// nothing here.
///
/// That is worth pinning in its own right, because it separates the two things a lookup does. The
/// router picks the shard; the per-shard `HashMap` then probes with the key's own `Hash`, which
/// `Borrow` already forces to agree. A regression that made a custom router influence the
/// intra-shard probe -- rather than only shard selection -- would show up here as a miss on a
/// single-shard store, and nowhere else in this file.
#[test]
fn a_disagreeing_router_costs_nothing_on_a_single_shard_store() {
    let c = ShardedUnboundCache::<UserId, u64>::builder()
        .shards(1)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap();
    assert_eq!(c.shards(), 1);
    for id in 0..KEYS {
        c.set(UserId(id), id * 10);
    }

    for id in 0..KEYS {
        assert_eq!(
            c.get(&id),
            Some(id * 10),
            "with one shard there is nowhere else for the borrowed form of `UserId({id})` to land"
        );
        assert_eq!(c.peek(&id), Some(id * 10));
        assert!(c.contains(&id));
    }
    for id in 0..KEYS {
        assert_eq!(c.remove_entry(&id), Some((UserId(id), id * 10)));
    }
    assert!(c.is_empty());
}

/// Generates the three per-store tests. Each store's builder expression is written out twice (once
/// per router) rather than passed through a closure, because the two expressions have different
/// concrete types and the point is that each store resolves its own inherent methods.
macro_rules! per_store_router_tests {
    ($name:ident, $label:literal, $agreeing:expr, $disagreeing:expr, $val:expr) => {
        mod $name {
            use super::*;

            /// The opt-in borrowed path, per store: with a second, agreeing `ShardHasher` impl,
            /// every owned insert is reachable through all six borrowed lookups. The borrowed
            /// lookups can only land on the shards the owned inserts went to if the borrowed path
            /// routes through the store's own `H`.
            #[test]
            fn an_agreeing_router_reaches_owned_inserts_through_every_borrowed_lookup() {
                let val = $val;
                let c = $agreeing;
                for id in 0..KEYS {
                    c.set(UserId(id), val(id));
                }
                assert_eq!(
                    c.len(),
                    KEYS as usize,
                    concat!($label, ": every owned insert must be stored")
                );
                let sizes = c.shard_sizes();
                assert!(
                    sizes.iter().filter(|n| **n > 0).count() > 1,
                    concat!(
                        $label,
                        ": a borrowed-routing check is only meaningful across several shards, saw {:?}"
                    ),
                    sizes
                );

                for id in 0..KEYS {
                    assert_eq!(
                        c.get(&id),
                        Some(val(id)),
                        concat!($label, ": borrowed `get` missed `UserId({})`"),
                        id
                    );
                    assert_eq!(
                        c.peek(&id),
                        Some(val(id)),
                        concat!($label, ": borrowed `peek` missed `UserId({})`"),
                        id
                    );
                    assert!(
                        c.contains(&id),
                        concat!($label, ": borrowed `contains` missed `UserId({})`"),
                        id
                    );
                }
                assert_eq!(c.get(&KEYS), None, concat!($label, ": absent key must miss"));

                // Each removing method gets its own third of the key space, so all three are
                // observed removing a real entry through the borrowed form.
                for id in 0..KEYS {
                    match id % 3 {
                        0 => assert_eq!(
                            c.remove(&id),
                            Some(val(id)),
                            concat!($label, ": borrowed `remove` missed `UserId({})`"),
                            id
                        ),
                        1 => assert_eq!(
                            c.remove_entry(&id),
                            Some((UserId(id), val(id))),
                            concat!(
                                $label,
                                ": borrowed `remove_entry` must hand back the stored owned key for `UserId({})`"
                            ),
                            id
                        ),
                        _ => assert!(
                            c.delete(&id),
                            concat!($label, ": borrowed `delete` missed `UserId({})`"),
                            id
                        ),
                    }
                }
                assert!(
                    c.is_empty(),
                    concat!($label, ": every entry must have been removed through the borrowed form")
                );
            }

            /// The contract's teeth, per store: a router whose impls disagree loses every present
            /// entry through every borrowed form, with no panic and no error, while the owned form
            /// keeps finding them.
            #[test]
            fn a_disagreeing_router_loses_present_entries_through_every_borrowed_lookup() {
                let val = $val;
                let c = $disagreeing;
                for id in 0..KEYS {
                    c.set(UserId(id), val(id));
                }
                assert_eq!(
                    c.len(),
                    KEYS as usize,
                    concat!($label, ": every owned insert must be stored")
                );

                for id in 0..KEYS {
                    assert_eq!(
                        c.get(&UserId(id)),
                        Some(val(id)),
                        concat!($label, ": the owned form must still find `UserId({})`"),
                        id
                    );
                    assert_eq!(
                        c.get(&id),
                        None,
                        concat!($label, ": borrowed `get` must MISS under a contract-violating router")
                    );
                    assert_eq!(
                        c.peek(&id),
                        None,
                        concat!($label, ": borrowed `peek` must miss")
                    );
                    assert!(
                        !c.contains(&id),
                        concat!($label, ": borrowed `contains` must miss")
                    );
                    assert_eq!(
                        c.remove(&id),
                        None,
                        concat!($label, ": borrowed `remove` must no-op")
                    );
                    assert_eq!(
                        c.remove_entry(&id),
                        None,
                        concat!($label, ": borrowed `remove_entry` must no-op")
                    );
                    assert!(
                        !c.delete(&id),
                        concat!($label, ": borrowed `delete` must no-op")
                    );
                }

                assert_eq!(
                    c.len(),
                    KEYS as usize,
                    concat!(
                        $label,
                        ": a contract-violating router loses entries to lookups, it does not delete them"
                    )
                );
            }

            /// Why the borrowed form missed above, reasoned from the store's own routing rather
            /// than observed on the borrowed lookup path: the shard an owned `UserId(id)` occupies
            /// and the shard a borrowed `&id` lookup is *expected* to consult -- which, per the
            /// router identity pinned at the top of this file, is wherever an owned
            /// `UserId(id ^ 1)` lands -- are different shards. No shard-index formula is
            /// recomputed here. The behavioral pin that the borrowed path actually misses is
            /// `a_disagreeing_router_loses_present_entries_through_every_borrowed_lookup` above.
            #[test]
            fn a_disagreeing_routers_two_forms_consult_different_shards() {
                let val = $val;
                for id in 0..SAMPLE {
                    let owned_home = {
                        let c = $disagreeing;
                        assert_eq!(c.shards(), SHARDS, concat!($label, ": shard count"));
                        c.set(UserId(id), val(id));
                        sole_occupied_shard(&c.shard_sizes(), $label)
                    };
                    let borrowed_target = {
                        let c = $disagreeing;
                        c.set(UserId(id ^ 1), val(id ^ 1));
                        sole_occupied_shard(&c.shard_sizes(), $label)
                    };
                    assert_ne!(
                        owned_home,
                        borrowed_target,
                        concat!(
                            $label,
                            ": `UserId({})` and the shard its borrowed `&{}` lookup consults must differ"
                        ),
                        id,
                        id
                    );
                }
            }
        }
    };
}

per_store_router_tests!(
    sharded_unbound,
    "ShardedUnboundCache",
    ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .hasher(AgreeingRouter)
        .build()
        .unwrap(),
    ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap(),
    |id: u64| id * 10
);

per_store_router_tests!(
    sharded_lru,
    "ShardedLruCache",
    ShardedLruCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(AgreeingRouter)
        .build()
        .unwrap(),
    ShardedLruCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap(),
    |id: u64| id * 10
);

per_store_router_tests!(
    sharded_expiring,
    "ShardedExpiringCache",
    ShardedExpiringCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .hasher(AgreeingRouter)
        .build()
        .unwrap(),
    ShardedExpiringCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap(),
    |id: u64| Live(id * 10)
);

per_store_router_tests!(
    sharded_expiring_lru,
    "ShardedExpiringLruCache",
    ShardedExpiringLruCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(AgreeingRouter)
        .build()
        .unwrap(),
    ShardedExpiringLruCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .max_size(MAX_SIZE)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap(),
    |id: u64| Live(id * 10)
);

#[cfg(feature = "time_stores")]
mod time_stores {
    use super::*;

    /// One hour: nothing under test may expire mid-run.
    const TTL: std::time::Duration = std::time::Duration::from_secs(3600);

    per_store_router_tests!(
        sharded_ttl,
        "ShardedTtlCache",
        ShardedTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .ttl(TTL)
            .hasher(AgreeingRouter)
            .build()
            .unwrap(),
        ShardedTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .ttl(TTL)
            .hasher(DisagreeingRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );

    per_store_router_tests!(
        sharded_lru_ttl,
        "ShardedLruTtlCache",
        ShardedLruTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .max_size(MAX_SIZE)
            .ttl(TTL)
            .hasher(AgreeingRouter)
            .build()
            .unwrap(),
        ShardedLruTtlCache::<UserId, u64>::builder()
            .shards(SHARDS)
            .max_size(MAX_SIZE)
            .ttl(TTL)
            .hasher(DisagreeingRouter)
            .build()
            .unwrap(),
        |id: u64| id * 10
    );
}
