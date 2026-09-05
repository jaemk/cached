// The third diagnostic shape for a missing `ShardHasher<Q>` impl, and the one a real custom-router
// author is most likely to hit after opting into borrowed lookups once.
//
// `sharded_router_missing_borrowed_impl.rs` pins the SINGLE-impl concrete router, where inference
// collapses `Q` onto the one impl that exists and rustc reports E0308 (a type mismatch), so
// `ShardHasher`'s `#[diagnostic::on_unimplemented]` never fires.
// `sharded_helper_missing_borrowed_bound.rs` pins the generic-helper case, where `H` is an
// unresolved type parameter and the message does fire.
//
// This file is the case in between: a CONCRETE hand-written router that already carries two
// `ShardHasher` impls, asked for a third key type it does not implement. With more than one impl
// available there is nothing for `Q` to collapse onto, so `Q` comes from the argument, the bound
// is genuinely unsatisfied, and the `on_unimplemented` text fires for a concrete router rather
// than only for a generic one.
use cached::{ShardHasher, ShardedUnboundCache};
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};

#[derive(PartialEq, Eq)]
struct Name(String);

// Hashes as its `str` form, as `Borrow`'s contract requires of every borrowed form below.
impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_str().hash(state);
    }
}

impl Borrow<String> for Name {
    fn borrow(&self) -> &String {
        &self.0
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

/// Deliberately not a `BuildHasher`. It routes `Name` and `String`, and opts into neither `str`
/// nor anything else.
#[derive(Clone)]
struct NameRouter;

impl ShardHasher<Name> for NameRouter {
    fn shard_hash(&self, key: &Name) -> u64 {
        key.0.len() as u64
    }
}

impl ShardHasher<String> for NameRouter {
    fn shard_hash(&self, key: &String) -> u64 {
        key.len() as u64
    }
}

fn main() {
    let cache: ShardedUnboundCache<Name, u32, NameRouter> = ShardedUnboundCache::builder()
        .hasher(NameRouter)
        .build()
        .expect("build");
    cache.set(Name("a".to_string()), 1);
    // `Q = str`: `Name: Borrow<str>` holds, but `NameRouter` has no `impl ShardHasher<str>`.
    let _ = cache.get("a");
}
