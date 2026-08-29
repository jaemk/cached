// The inherent `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` on the sharded stores are
// bounded on `BorrowedKeyRouting`, which is exactly `BuildHasher`. That bound is unconditional
// rather than predicated on `Q != K`, so the plain OWNED-key call `cache.get(&k)` fails the same
// way a borrowed-key call does. This is the dominant real-world break: roughly 20 call sites
// across four test files had to be rewritten to the trait-method form when the bound landed.
//
// This golden pins the `#[diagnostic::on_unimplemented]` text on `BorrowedKeyRouting` for that
// owned-key case specifically. The borrowed-key case has its own golden in
// `sharded_borrowed_key_requires_build_hasher.rs`.
use cached::{ShardHasher, ShardedUnboundCache};

/// A hand-written `ShardHasher`. Coherence keeps it from also being a `BuildHasher`, which is
/// precisely why it has no borrowed-key (or owned-key) routing.
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
    let k = "a".to_string();
    cache.set(k.clone(), 1);
    // Owned key (`&String` for a `String`-keyed store): still requires `H: BorrowedKeyRouting`.
    let _ = cache.get(&k);
}
