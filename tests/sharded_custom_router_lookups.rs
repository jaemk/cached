//! A hand-written, non-`BuildHasher` [`cached::ShardHasher`] router driving the six inherent
//! lookups on the five sharded stores the crate-root doc module does not cover, plus the opt-in
//! borrowed path over a newtype key and the cross-impl consistency contract's failure mode.
//!
//! This is the regression net for design record 0055
//! (`specs/design/0055-shard-hasher-q-over-borrowed-key-routing.md`). Every call made through a
//! hand-written router below -- which is all of them except the closing `&[u8]` case, whose store
//! uses the default hasher -- fails to compile under the superseded `H: BorrowedKeyRouting`
//! bound, because that bound covered every caller of the one method per name, owned-key calls
//! included. Break class 2 (`cache.get(&k)` with `k: &K` inferring `Q = &K`) is not fixed by 0055
//! and nothing here implies it is; its coverage lives in
//! `tests/sharded_generic_helper_bounds.rs`.
//!
//! ## What is deliberately NOT duplicated here
//!
//! - `src/lib.rs`, `mod custom_shard_hasher_doc_contract` (end of file): the six inherent owned
//!   lookups over a hand-written router on `ShardedLruCache`, the documented generic-helper
//!   snippet, and a two-impl `String`/`str` opt-in router. `ShardedLruCache` is therefore the one
//!   store absent from the break-class-1 sweep below.
//! - `src/stores/sharded/mod.rs`, `mod tests`: `ShardHasher<String>` vs `ShardHasher<str>`
//!   agreement on the blanket impl, and a two-impl hand-written router's coherence at the hasher
//!   level (no store involved).
//! - Each store module's in-module `borrowed_key_and_capability_tests`: blanket-impl borrowed
//!   lookups on all six stores, including `&str` -> `Q = str` inference through all six methods
//!   and `Vec<u8>` -> `&[u8]` routing parity plus a borrowed `get`. Only the `&[u8]` methods
//!   those tests leave out are picked up below.
//! - `tests/sharded_newtype_key_routing_parity.rs`: newtype-key routing parity through the
//!   *blanket* impl (default and hand-written `BuildHasher`s). The newtype cases here use a
//!   hand-written router instead, which that file never builds.

use std::borrow::Borrow;
use std::hash::{Hash, Hasher};

use cached::{
    Expires, ShardHasher, ShardedExpiringCache, ShardedExpiringLruCache, ShardedUnboundCache,
};

#[cfg(feature = "time_stores")]
use cached::{ShardedLruTtlCache, ShardedTtlCache};

/// 2^64 / phi. Spreads entropy into the upper 32 bits, which is the half shard selection reads.
const PHI: u64 = 0x9e37_79b9_7f4a_7c15;

/// A router that is deliberately **not** a `BuildHasher`, implementing `ShardHasher` for the
/// owned key type and nothing else. Under 0052's `H: BorrowedKeyRouting` bound a store built on
/// this type had no inherent `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` at all --
/// not even for owned keys -- and every call in the five tests below was a compile error.
#[derive(Clone)]
struct OwnedOnlyRouter;

impl ShardHasher<u64> for OwnedOnlyRouter {
    fn shard_hash(&self, key: &u64) -> u64 {
        key.wrapping_mul(PHI)
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

/// The six inherent owned-key lookups, exercised as a fixed script so every store below is held
/// to the same list. Written out per store rather than behind a trait because the point is that
/// each concrete type resolves its own inherent methods.
macro_rules! assert_six_owned_lookups {
    ($c:expr, $mk:expr, $store:literal) => {{
        let c = &$c;
        let mk = $mk;
        for id in 1u64..=6 {
            c.set(id, mk(id));
        }
        assert_eq!(c.len(), 6, "{} must hold every owned insert", $store);

        assert_eq!(c.get(&1), Some(mk(1)), "{}: owned `get`", $store);
        assert_eq!(c.peek(&2), Some(mk(2)), "{}: owned `peek`", $store);
        assert!(c.contains(&3), "{}: owned `contains`", $store);
        assert_eq!(c.remove(&4), Some(mk(4)), "{}: owned `remove`", $store);
        assert_eq!(
            c.remove_entry(&5),
            Some((5u64, mk(5))),
            "{}: owned `remove_entry`",
            $store
        );
        assert!(c.delete(&6), "{}: owned `delete`", $store);

        assert!(!c.contains(&4), "{}: `remove` must remove", $store);
        assert!(!c.contains(&5), "{}: `remove_entry` must remove", $store);
        assert!(!c.contains(&6), "{}: `delete` must remove", $store);
        assert_eq!(c.get(&7), None, "{}: absent key must miss", $store);
        assert_eq!(c.len(), 3, "{} must have dropped exactly three", $store);
    }};
}

#[test]
fn hand_written_router_keeps_the_six_inherent_lookups_on_sharded_unbound() {
    let c = ShardedUnboundCache::<u64, u64>::builder()
        .shards(4)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap();
    assert_six_owned_lookups!(c, |id: u64| id * 10, "ShardedUnboundCache");
}

#[test]
fn hand_written_router_keeps_the_six_inherent_lookups_on_sharded_expiring() {
    let c = ShardedExpiringCache::<u64, Live>::builder()
        .shards(4)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap();
    assert_six_owned_lookups!(c, |id: u64| Live(id * 10), "ShardedExpiringCache");
}

#[test]
fn hand_written_router_keeps_the_six_inherent_lookups_on_sharded_expiring_lru() {
    let c = ShardedExpiringLruCache::<u64, Live>::builder()
        .shards(4)
        .max_size(256)
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap();
    assert_six_owned_lookups!(c, |id: u64| Live(id * 10), "ShardedExpiringLruCache");
}

#[cfg(feature = "time_stores")]
#[test]
fn hand_written_router_keeps_the_six_inherent_lookups_on_sharded_ttl() {
    let c = ShardedTtlCache::<u64, u64>::builder()
        .shards(4)
        .ttl(std::time::Duration::from_secs(3600))
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap();
    assert_six_owned_lookups!(c, |id: u64| id * 10, "ShardedTtlCache");
}

#[cfg(feature = "time_stores")]
#[test]
fn hand_written_router_keeps_the_six_inherent_lookups_on_sharded_lru_ttl() {
    let c = ShardedLruTtlCache::<u64, u64>::builder()
        .shards(4)
        .max_size(256)
        .ttl(std::time::Duration::from_secs(3600))
        .hasher(OwnedOnlyRouter)
        .build()
        .unwrap();
    assert_six_owned_lookups!(c, |id: u64| id * 10, "ShardedLruTtlCache");
}

// ---------------------------------------------------------------------------------------------
// The opt-in borrowed path: a second, agreeing `ShardHasher` impl on the same router.
// ---------------------------------------------------------------------------------------------

/// A newtype over a primitive, borrowable as that primitive. `Hash` delegates to the inner `u64`,
/// as `Borrow`'s contract requires, so the per-shard `HashMap` probe agrees for both forms and
/// only the router decides whether the two forms meet.
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

/// A router with one impl per key type it routes. The two agree on keys that compare equal --
/// `shard_hash(&UserId(id))` equals `shard_hash(&id)` for every `id` -- which is the contract
/// `ShardHasher`'s rustdoc places on a multi-impl router and which the compiler cannot check.
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

const SHARDS: usize = 16;
const KEYS: u64 = 256;

fn agreeing_router_store() -> ShardedUnboundCache<UserId, u64, AgreeingRouter> {
    let c = ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .hasher(AgreeingRouter)
        .build()
        .unwrap();
    for id in 0..KEYS {
        c.set(UserId(id), id * 10);
    }
    let sizes = c.shard_sizes();
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "a borrowed-routing check is only meaningful across several shards, saw {sizes:?}"
    );
    c
}

/// The opt-in half of the contract, over a newtype key rather than the `String`/`str` pair the
/// crate-root doc module covers: an owned `set(UserId(id), _)` is reachable through the borrowed
/// `&u64` form, on every one of the lookups that takes a borrowed key.
#[test]
fn a_second_agreeing_impl_makes_owned_inserts_reachable_through_the_borrowed_form() {
    let c = agreeing_router_store();

    // The single-key statement the `ShardHasher` rustdoc makes, spelled out.
    assert_eq!(c.get(&7u64), Some(70), "borrowed `get` missed `UserId(7)`");

    for id in 0..KEYS {
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
}

/// The removing half of the same opt-in path. `remove_entry` hands back the STORED owned key,
/// which is the only way to observe that the borrowed lookup reached the entry the owned insert
/// created rather than an equal-comparing stand-in.
#[test]
fn borrowed_removals_through_a_second_agreeing_impl_reach_the_owned_entries() {
    let c = agreeing_router_store();
    let half = KEYS / 2;

    for id in 0..half {
        assert_eq!(
            c.remove_entry(&id),
            Some((UserId(id), id * 10)),
            "borrowed `remove_entry` must hand back the stored owned key for `UserId({id})`"
        );
    }
    for id in half..KEYS {
        assert_eq!(
            c.remove(&id),
            Some(id * 10),
            "borrowed `remove` missed `UserId({id})`"
        );
        assert!(
            !c.delete(&id),
            "a `delete` after the borrowed `remove` of `UserId({id})` must report nothing"
        );
    }
    assert!(
        c.is_empty(),
        "every entry must have been removed through the borrowed form"
    );

    // `delete` on its own, on a fresh fill, so its `true` return is observed too.
    let c = agreeing_router_store();
    for id in 0..KEYS {
        assert!(
            c.delete(&id),
            "borrowed `delete` must report the removal of `UserId({id})`"
        );
    }
    assert!(c.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The contract's teeth: what a router that violates it actually costs.
// ---------------------------------------------------------------------------------------------

/// A router whose two impls DISAGREE. This is a deliberate violation of the consistency contract
/// documented on `ShardHasher`, written here to pin the documented consequence -- **not** as a
/// pattern to copy. Real routers must satisfy `shard_hash(&k) == shard_hash(k.borrow())`, as
/// `AgreeingRouter` above does.
///
/// The disagreement is arranged to be deterministic rather than probabilistic: each impl places
/// the routing bits directly in the upper half (`x << 32`), so the shard index the store computes
/// as `(hash >> 32) & mask` is `id & mask` for the owned form and `(id ^ 1) & mask` for the
/// borrowed one. Those differ in bit 0 for every `id` and every mask, so the two forms land on
/// different shards for every key on any run.
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

/// The index of the one occupied shard of a freshly built store holding exactly one entry, read
/// from the public `shard_sizes()` rather than the shard-index formula `ShardHasher`'s rustdoc
/// documents.
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

/// Pins the footgun `ShardHasher`'s rustdoc describes but nothing else checks: when a router's
/// impls disagree, an owned insert and its equivalent borrowed lookup route to different shards,
/// so the borrowed form reports a miss on an entry the cache still holds and the borrowed
/// removals silently no-op. There is no panic and no error.
///
/// This test asserts that the violation IS observable. It documents a hazard; it does not endorse
/// one. A router that satisfies the contract behaves as
/// `a_second_agreeing_impl_makes_owned_inserts_reachable_through_the_borrowed_form` shows.
#[test]
fn disagreeing_router_impls_lose_a_present_entry_through_its_borrowed_form() {
    let c = ShardedUnboundCache::<UserId, u64>::builder()
        .shards(SHARDS)
        .hasher(DisagreeingRouter)
        .build()
        .unwrap();
    assert_eq!(c.shards(), SHARDS);

    // Why it happens, before what happens: the two impls send the same key to different shards.
    // Read off the store's own routing through `shard_sizes()`, on a sample of keys, rather than
    // by recomputing the shard-index formula: a fresh single-entry store's one occupied shard is
    // exactly where a lookup for that key would land.
    for id in 0..8u64 {
        let owned_home = {
            let s = ShardedUnboundCache::<UserId, u64>::builder()
                .shards(SHARDS)
                .hasher(DisagreeingRouter)
                .build()
                .unwrap();
            s.set(UserId(id), id * 10);
            sole_occupied_shard(&s.shard_sizes(), "DisagreeingRouter (owned)")
        };
        let borrowed_target = {
            let s = ShardedUnboundCache::<UserId, u64>::builder()
                .shards(SHARDS)
                .hasher(DisagreeingRouter)
                .build()
                .unwrap();
            s.set(UserId(id ^ 1), (id ^ 1) * 10);
            sole_occupied_shard(&s.shard_sizes(), "DisagreeingRouter (borrowed target)")
        };
        assert_ne!(
            owned_home, borrowed_target,
            "this test needs the two impls to disagree for `UserId({id})`"
        );
    }

    for id in 0..KEYS {
        c.set(UserId(id), id * 10);
    }
    assert_eq!(c.len(), KEYS as usize, "every owned insert is stored");

    for id in 0..KEYS {
        // The entry is present and reachable through the form it was inserted with.
        assert_eq!(
            c.get(&UserId(id)),
            Some(id * 10),
            "the owned form must still find `UserId({id})`"
        );

        // The borrowed form lands on the wrong shard: a miss on an entry that is present.
        assert_eq!(
            c.get(&id),
            None,
            "borrowed `get` is expected to MISS under a contract-violating router"
        );
        assert_eq!(c.peek(&id), None, "borrowed `peek` is expected to miss");
        assert!(!c.contains(&id), "borrowed `contains` is expected to miss");

        // And the removals silently no-op rather than removing anything.
        assert_eq!(
            c.remove(&id),
            None,
            "borrowed `remove` is expected to no-op"
        );
        assert_eq!(
            c.remove_entry(&id),
            None,
            "borrowed `remove_entry` is expected to no-op"
        );
        assert!(!c.delete(&id), "borrowed `delete` is expected to no-op");
    }

    // Nothing was removed by any of that: the cache still holds everything it was given.
    assert_eq!(
        c.len(),
        KEYS as usize,
        "a contract-violating router loses entries to lookups, it does not delete them"
    );
}

// ---------------------------------------------------------------------------------------------
// `?Sized` beyond `str`: a `Vec<u8>`-keyed store looked up through `&[u8]`.
// ---------------------------------------------------------------------------------------------

/// Byte-slice keys through the blanket `BuildHasher` impl, which reaches `ShardHasher<[u8]>` only
/// because the trait parameter is `?Sized`. The per-store in-module tests pin `Vec<u8>` /`&[u8]`
/// routing parity and a borrowed `get`; the other five lookups through `&[u8]` are covered here,
/// from outside the crate.
#[test]
fn byte_slice_lookups_reach_owned_vec_keys_on_every_inherent_method() {
    let c = ShardedUnboundCache::<Vec<u8>, u32>::builder()
        .shards(8)
        .build()
        .unwrap();
    let keys: Vec<Vec<u8>> = (0..200u32)
        .map(|i| format!("key-{i}").into_bytes())
        .collect();
    for (i, k) in keys.iter().enumerate() {
        c.set(k.clone(), i as u32);
    }
    let sizes = c.shard_sizes();
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "byte-key lookups are only meaningful across several shards, saw {sizes:?}"
    );

    for (i, k) in keys.iter().enumerate() {
        let q: &[u8] = k.as_slice();
        assert!(c.contains(q), "borrowed `contains` missed `{k:?}`");
        assert_eq!(c.peek(q), Some(i as u32), "borrowed `peek` missed `{k:?}`");
        assert_eq!(c.get(q), Some(i as u32), "borrowed `get` missed `{k:?}`");
    }
    assert_eq!(c.get(b"absent".as_slice()), None);

    let half = keys.len() / 2;
    for (i, k) in keys.iter().enumerate().take(half) {
        assert_eq!(
            c.remove_entry(k.as_slice()),
            Some((k.clone(), i as u32)),
            "borrowed `remove_entry` must hand back the stored owned `Vec<u8>` key"
        );
    }
    for (i, k) in keys.iter().enumerate().skip(half) {
        assert_eq!(
            c.remove(k.as_slice()),
            Some(i as u32),
            "borrowed `remove` missed `{k:?}`"
        );
        assert!(
            !c.delete(k.as_slice()),
            "a `delete` after the borrowed `remove` of `{k:?}` must report nothing"
        );
    }
    assert!(
        c.is_empty(),
        "every entry must have been removed by `&[u8]`"
    );
}
