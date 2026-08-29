//! Owned and borrowed keys must reach the same shard for a newtype over a primitive.
//!
//! `struct UserId(u64)` with `Borrow<u64>` and a `Hash` that delegates to the inner `u64` is the
//! shape that catches a routing hash which dispatches on the static type of what it hashes. An
//! entry inserted with the owned `set(UserId(id), v)` and read back with a borrowed `get(&id)`
//! must land on one shard, or the entry is present and permanently invisible: `get` misses
//! forever (re-running whatever backs the cache), and `remove`/`delete` report nothing removed
//! while the entry stays live.
//!
//! ## What could break this, and when it is observable
//!
//! Sharded routing used to go through [`std::hash::BuildHasher::hash_one`]. That is an
//! overridable *provided* method whose implementation is permitted to dispatch on its static type
//! argument `T`, so `hash_one::<&UserId>` and `hash_one::<&u64>` are not obliged to agree even
//! though the `Borrow` contract makes the two values hash identically. `ahash::RandomState` does
//! exactly that dispatch: it routes through a `CallHasher` table carrying specialized impls for
//! the reference types `&u8`..`&i64` and `&u128`/`&i128`/`&usize`/`&isize`, and none for
//! `&str`/`&String`/`&[u8]`/`&Vec<u8>` or for a user newtype. Both routing paths now build a
//! `Hasher`, feed the key to it and finish it, so they depend on the `Hash` impl alone.
//!
//! ahash only compiles those specialized impls when its `specialize` cfg is on, which its
//! build.rs enables on any nightly rustc. On a stable toolchain the divergence this file guards
//! is therefore not reachable, and these tests are a structural guard rather than a detector.
//! They fail on any toolchain if a routing path is changed back to `hash_one` *and* the
//! specialized path is active. Keeping them cheap and unconditional is deliberate: the cost of a
//! silent permanent miss is high and the test is a few hundred hash computations.
//!
//! The existing `String`/`&str` and `Vec<u8>`/`&[u8]` parity tests provably cannot catch this,
//! because both sides of those pairs take ahash's generic path.

use std::borrow::Borrow;
use std::hash::{Hash, Hasher};

use cached::{
    Expires, ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache, ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// 16 shards over 512 keys: enough that a divergent borrowed route lands on the right shard by
/// coincidence for only about one key in sixteen, so at least one of the 512 assertions fires.
const SHARDS: usize = 16;
const KEYS: u64 = 512;

/// A newtype over a primitive, borrowable as that primitive. `Hash` delegates to the inner
/// `u64`, which is what `Borrow`'s contract requires: `UserId(id)` and `id` hash identically.
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

fn ids() -> impl Iterator<Item = u64> {
    0..KEYS
}

/// Every store is filled through the owned key and then read back through the borrowed one, so a
/// helper cannot be shared across the six concrete types without a trait of its own. Each test
/// below follows this identical sequence:
///
/// 1. `set(UserId(id), value)` for every id, owned.
/// 2. assert the fill spans more than one shard (a single-shard cache cannot detect a mismatch).
/// 3. borrowed `contains(&id)` / `peek(&id)` / `get(&id)` all find the entry.
/// 4. borrowed `remove(&id)` hands the value back, and the entry is then gone.
fn assert_spans_shards(sizes: &[usize]) {
    assert_eq!(
        sizes.len(),
        SHARDS,
        "the parity check needs the shard count it asked for"
    );
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "routing parity is only meaningful across several shards, saw {sizes:?}"
    );
    assert_eq!(
        sizes.iter().sum::<usize>(),
        KEYS as usize,
        "every owned insert must be stored exactly once"
    );
}

#[test]
fn sharded_unbound_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), id * 10);
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(id * 10),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(id * 10),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(id * 10),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

#[test]
fn sharded_lru_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedLruCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .max_size(4096)
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), id * 10);
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(id * 10),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(id * 10),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(id * 10),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

#[test]
fn sharded_expiring_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedExpiringCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), Live(id * 10));
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(Live(id * 10)),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(Live(id * 10)),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(Live(id * 10)),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

#[test]
fn sharded_expiring_lru_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedExpiringLruCache::<UserId, Live>::builder()
        .shards(SHARDS)
        .max_size(4096)
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), Live(id * 10));
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(Live(id * 10)),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(Live(id * 10)),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(Live(id * 10)),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedTtlCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .ttl(std::time::Duration::from_secs(3600))
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), id * 10);
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(id * 10),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(id * 10),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(id * 10),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_lru_ttl_routes_a_newtype_key_the_same_owned_and_borrowed() {
    let c = ShardedLruTtlCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .max_size(4096)
        .ttl(std::time::Duration::from_secs(3600))
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), id * 10);
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids() {
        assert!(c.contains(&id), "borrowed `contains` missed `UserId({id})`");
        assert_eq!(
            c.peek(&id),
            Some(id * 10),
            "borrowed `peek` missed `UserId({id})`"
        );
        assert_eq!(
            c.get(&id),
            Some(id * 10),
            "borrowed `get` missed `UserId({id})`"
        );
    }
    for id in ids() {
        assert_eq!(
            c.remove(&id),
            Some(id * 10),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.contains(&id),
            "`UserId({id})` must be gone after a borrowed `remove`"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}

/// `remove_entry` and `delete` share the borrowed routing with `get`/`remove`, but report
/// differently: `remove_entry` hands back the STORED owned key, which is the only way to observe
/// that the borrowed lookup found the entry the owned insert created rather than an equal one.
#[test]
fn borrowed_remove_entry_and_delete_reach_the_owned_newtype_entry() {
    let c = ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    for id in ids() {
        c.set(UserId(id), id * 10);
    }
    assert_spans_shards(&c.shard_sizes());

    for id in ids().take(KEYS as usize / 2) {
        assert_eq!(
            c.remove_entry(&id),
            Some((UserId(id), id * 10)),
            "borrowed `remove_entry` must hand back the stored owned key for `UserId({id})`"
        );
    }
    for id in ids().skip(KEYS as usize / 2) {
        assert!(
            c.delete(&id),
            "borrowed `delete` must report the removal of `UserId({id})`"
        );
        assert!(
            !c.delete(&id),
            "a second borrowed `delete` of `UserId({id})` must report nothing"
        );
    }
    assert!(c.is_empty(), "every entry must have been removed");
}
