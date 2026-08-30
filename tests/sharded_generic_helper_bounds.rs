//! Compiles the generic-helper bound sets documented on `src/lib.rs` (the "Custom shard hashers"
//! blockquote) rather than merely asserting them in prose. If the documented bound set stops
//! being the actual working bound set, this file fails to compile.
//!
//! Two of the three breaking changes 0052 introduced are gone under 0055: a helper doing owned
//! lookups needs no marker bound beyond `H: ShardHasher<K>` (break class 3), and a hand-written
//! router keeps its inherent methods (break class 1, pinned in
//! `tests/sharded_custom_router_lookups.rs`). Break class 2 -- `cache.get(&k)` with `k: &K`
//! inferring `Q = &K` -- is **not** fixed and is not claimed to be: it follows from making the
//! methods generic over `Q` at all. `lookup_all` below is that surviving sharp edge, written the
//! documented way.

use cached::{ShardHasher, ShardedLruCache};
use std::borrow::Borrow;
use std::hash::Hash;

/// The full bound set the docs give as the working signature for a generic helper that calls the
/// inherent `get` with an owned key, copied from the `src/lib.rs` blockquote verbatim: the
/// key/value bounds the `impl` block itself requires (`K: Hash + Eq + Clone`, `V: Clone`) plus
/// `H: ShardHasher<K>`, which is exactly the bound the owned-key `get` carries. No marker trait
/// is involved. Adding one back to the method would break this definition, not just its callers.
fn lookup<K, V, H>(c: &ShardedLruCache<K, V, H>, k: &K) -> Option<V>
where
    K: Hash + Eq + Clone,
    V: Clone,
    H: ShardHasher<K>,
{
    c.get(k)
}

/// A helper that looks keys up in a *borrowed* form names that form in its own bound, as the docs
/// say: `H: ShardHasher<str>` for a `c.get("a")` on a `ShardedLruCache<String, V, H>`. That is the
/// bound the borrowed call adds; `H: ShardHasher<String>` is still carried because the inherent
/// `get` lives in an `impl` block bounded on `H: ShardHasher<K>`, so the borrowed form is an
/// addition to the store's own hasher bound rather than a replacement for it.
fn lookup_borrowed<V, H>(c: &ShardedLruCache<String, V, H>, k: &str) -> Option<V>
where
    V: Clone,
    H: ShardHasher<String> + ShardHasher<str>,
{
    c.get(k)
}

/// The same shape with the borrowed form left generic: `Q` is the looked-up form, and the hasher
/// bound names it alongside the store's key type. `Q: ?Sized` is reachable only because
/// `ShardHasher<K: ?Sized>` relaxed its parameter; under a `Sized` parameter this signature would
/// not accept `Q = str`.
fn lookup_generic_borrowed<K, V, H, Q>(c: &ShardedLruCache<K, V, H>, k: &Q) -> Option<V>
where
    K: Hash + Eq + Clone + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
    V: Clone,
    H: ShardHasher<K> + ShardHasher<Q>,
{
    c.get(k)
}

/// The argument-inference case: a loop variable that is a reference to the key type must be
/// passed to `get` without an extra `&`, since `get`'s `&Q` parameter already takes a reference.
/// `cache.get(&k)` where `k: &String` infers `Q = &String` and fails to compile; `cache.get(k)`
/// infers `Q = String` and works. This break is not fixed by `ShardHasher<Q>` routing -- it comes
/// from `Q` being inferred at the call site at all -- so the coverage stays.
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
fn borrowed_form_helper_naming_shard_hasher_str_compiles_and_works() {
    let cache: ShardedLruCache<String, u32> = ShardedLruCache::new(64);
    cache.set("a".to_string(), 1);

    assert_eq!(lookup_borrowed(&cache, "a"), Some(1));
    assert_eq!(lookup_borrowed(&cache, "b"), None);
    // The same store, reached through the fully generic borrowed helper: `Q = str` inferred from
    // a `&str` argument, and `Q = String` from an owned key's reference.
    assert_eq!(lookup_generic_borrowed(&cache, "a"), Some(1));
    assert_eq!(lookup_generic_borrowed(&cache, &"a".to_string()), Some(1));
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
