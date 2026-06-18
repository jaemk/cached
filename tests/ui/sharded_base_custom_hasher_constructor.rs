// `new` and `builder` exist only on the default-hasher specialization of each sharded
// `*Base` type, so a `Base::<_, _, CustomHasher>::{new,builder}()` turbofish (which would
// silently drop the custom hasher) does not compile. A custom hasher is introduced via
// `ShardedUnboundCache::builder().hasher(h)` instead, which switches the builder's hasher type.
use cached::{ShardHasher, ShardedUnboundCacheBase};

#[derive(Default)]
struct ConstHasher;
impl ShardHasher<u32> for ConstHasher {
    fn shard_hash(&self, _key: &u32) -> u64 {
        0
    }
}

fn main() {
    let _ = ShardedUnboundCacheBase::<u32, u32, ConstHasher>::builder();
    let _ = ShardedUnboundCacheBase::<u32, u32, ConstHasher>::new();
}
