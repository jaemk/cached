// Per specs/design/0055 (Pitfalls): a hand-written router that implements exactly ONE
// `ShardHasher<K>` makes an otherwise-missing borrowed-key impl collapse to an inference / type
// mismatch (E0308) rather than an unsatisfied-trait-bound error (E0277). With only one impl
// available, rustc resolves `Q` to it and reports the borrowed-key argument itself as mismatched,
// so `ShardHasher`'s `#[diagnostic::on_unimplemented]` message never fires here -- there is no
// unsatisfied bound for it to attach to. See `sharded_helper_missing_borrowed_bound.rs` for the
// case where the message DOES fire (a generic helper, not a concrete router with one impl).
use cached::{ShardHasher, ShardedUnboundCache};
use std::borrow::Borrow;

#[derive(Hash, PartialEq, Eq)]
struct UserId(u64);

impl Borrow<u64> for UserId {
    fn borrow(&self) -> &u64 {
        &self.0
    }
}

/// Deliberately not a `BuildHasher`, and deliberately carries only ONE `ShardHasher` impl: no
/// `ShardHasher<u64>` to route the borrowed lookup below.
#[derive(Clone)]
struct TenantRouter;

impl ShardHasher<UserId> for TenantRouter {
    fn shard_hash(&self, key: &UserId) -> u64 {
        key.0
    }
}

fn main() {
    let cache: ShardedUnboundCache<UserId, u32, TenantRouter> = ShardedUnboundCache::builder()
        .hasher(TenantRouter)
        .build()
        .expect("build");
    cache.set(UserId(7), 9);
    // Borrowed key (`&u64` for a `UserId`-keyed store). `TenantRouter` has no `ShardHasher<u64>`
    // impl, but the error below is not about that: with `Q = UserId` the only type that fits,
    // rustc infers `Q = UserId` from the sole impl and reports this argument as mismatched.
    assert_eq!(cache.get(&7u64), Some(9));
}
