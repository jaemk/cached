// The borrowed-key inherent lookups on the sharded stores (`get`/`remove`/`remove_entry`/
// `delete`/`contains`/`peek`) are bounded on `BorrowedKeyRouting`, which is exactly
// `BuildHasher`. A store built on a hand-written `ShardHasher` carries no agreement between the
// owned key's routing hash and a borrowed key's, so those methods must not resolve for it.
//
// This golden pins the `#[diagnostic::on_unimplemented]` text on `BorrowedKeyRouting`: without
// it, a regression that drops the attribute would silently degrade the error to a bare `E0277`
// naming `BuildHasher`, which says nothing about shard routing or about the owned-key call
// (`ConcurrentCachedExt::get(&cache, &key)`) that does work.
use cached::{ShardHasher, ShardedUnboundCache};

/// A hand-written `ShardHasher`. Coherence keeps it from also being a `BuildHasher`, which is
/// precisely why it has no borrowed-key routing.
#[derive(Clone)]
struct ByteSumHasher;

impl ShardHasher<String> for ByteSumHasher {
    fn shard_hash(&self, key: &String) -> u64 {
        key.bytes().map(u64::from).sum()
    }
}

fn main() {
    let cache: ShardedUnboundCache<String, u32, ByteSumHasher> = ShardedUnboundCache::builder()
        .hasher(ByteSumHasher)
        .build()
        .expect("build");
    cache.set("a".to_string(), 1);
    // Borrowed key (`&str` for a `String`-keyed store): requires `H: BorrowedKeyRouting`.
    let _ = cache.get("a");
}
