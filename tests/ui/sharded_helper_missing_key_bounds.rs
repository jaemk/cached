// The generic-helper failure the crate docs describe: naming a sharded store's third type
// parameter in your own helper is not the sharp edge, the KEY bounds are. `ShardedLruCache`'s
// inherent `get` lives in an `impl` block bounded on `K: Hash + Eq + Clone` (and the method
// itself returns `V` by value, so `V: Clone`). A helper that carries only `H: ShardHasher<K>`
// therefore fails at its own DEFINITION, regardless of what any call site passes, and rustc
// reports it as E0599 "no method named `get`" with the missing bounds listed rather than as a
// missing hasher impl.
//
// The working signature is the same function plus `where K: Hash + Eq + Clone, V: Clone`; see
// `tests/sharded_generic_helper_bounds.rs` for the compiling counterpart.
use cached::{ShardHasher, ShardedLruCache};

fn lookup<K, V, H: ShardHasher<K>>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V> {
    c.get(k)
}

fn main() {
    let cache: ShardedLruCache<String, u32> = ShardedLruCache::new(16);
    cache.set("a".to_string(), 1);
    let _ = lookup(&cache, &"a".to_string());
}
