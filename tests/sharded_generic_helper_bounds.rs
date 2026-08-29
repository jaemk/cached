//! Compiles the generic-helper bound sets documented on `src/lib.rs` (the "Custom shard hashers"
//! blockquote) rather than merely asserting them in prose. If the documented bound set stops
//! being the actual working bound set, this file fails to compile.

use cached::{BorrowedKeyRouting, ShardHasher, ShardedLruCache};
use std::hash::Hash;

/// The full bound set the docs say a generic helper needs to call the inherent `get`: the
/// key/value bounds the `impl` block itself requires (`K: Hash + Eq + Clone`, `V: Clone`) plus
/// `H: ShardHasher<K> + BorrowedKeyRouting` for the method's own `H: BorrowedKeyRouting` bound.
/// `H: ShardHasher<K>` alone is not enough -- that is the documented sharp edge.
fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K> + BorrowedKeyRouting,
{
    c.get(k)
}

/// The argument-inference case: a loop variable that is a reference to the key type must be
/// passed to `get` without an extra `&`, since `get`'s `&Q` parameter already takes a reference.
/// `cache.get(&k)` where `k: &String` infers `Q = &String` and fails to compile; `cache.get(k)`
/// infers `Q = String` and works.
fn lookup_all<V: Clone>(cache: &ShardedLruCache<String, V>, keys: &[String]) -> Vec<Option<V>> {
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        // `k: &String` here. The documented fix is to pass it directly, not `&k`.
        out.push(cache.get(k));
    }
    out
}

#[test]
fn generic_helper_with_documented_bounds_compiles_and_works() {
    let cache: ShardedLruCache<u64, u64> = ShardedLruCache::new(64);
    cache.set(1, 100);
    assert_eq!(lookup(&cache, &1), Some(100));
    assert_eq!(lookup(&cache, &2), None);
}

#[test]
fn dropping_the_extra_reference_compiles_and_finds_every_key() {
    let cache: ShardedLruCache<String, u32> = ShardedLruCache::new(64);
    let keys: Vec<String> = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    for (i, k) in keys.iter().enumerate() {
        cache.set(k.clone(), i as u32);
    }
    let found = lookup_all(&cache, &keys);
    assert_eq!(found, vec![Some(0), Some(1), Some(2)]);
}
