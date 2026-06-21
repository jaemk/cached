// `ShardHasher` has `Clone` as a supertrait (item 11). A custom hasher type
// that does NOT implement `Clone` must be rejected: the `impl ShardHasher` is
// only valid for `Clone` types. This locks the supertrait contract so the
// sharded stores can rely on cloning the hasher across shards/threads.
use cached::ShardHasher;

// Intentionally NOT `#[derive(Clone)]`.
struct NonCloneHasher;

impl ShardHasher<u64> for NonCloneHasher {
    fn shard_hash(&self, key: &u64) -> u64 {
        *key
    }
}

fn main() {}
