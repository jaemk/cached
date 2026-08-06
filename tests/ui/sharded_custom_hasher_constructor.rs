// `new` and `builder` exist only on the default-hasher instantiation of each sharded store
// (`ShardedX<K, V, DefaultShardHasher>`), so a `ShardedX::<_, _, CustomHasher>::{new,builder}()`
// turbofish (which would silently drop the custom hasher) does not compile. A custom hasher is
// introduced via `ShardedUnboundCache::builder().hasher(h)` instead, which switches the builder's
// hasher type.
use cached::{ShardHasher, ShardedUnboundCache};

#[derive(Clone, Default)]
struct ConstHasher;
impl ShardHasher<u32> for ConstHasher {
    fn shard_hash(&self, _key: &u32) -> u64 {
        0
    }
}

fn main() {
    let _ = ShardedUnboundCache::<u32, u32, ConstHasher>::builder();
    let _ = ShardedUnboundCache::<u32, u32, ConstHasher>::new();
}
