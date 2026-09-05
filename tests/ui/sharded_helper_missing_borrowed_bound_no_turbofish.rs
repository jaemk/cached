// The plain-call twin of `sharded_helper_missing_borrowed_bound.rs`, and the golden behind the one
// migration claim in the crate-level "Custom shard hashers" blockquote (`src/lib.rs`, and the
// byte-identical paragraph in `README.md`) that nothing else pins: with only
// `H: ShardHasher<String>` in scope, `c.get("a")` fails as an E0308 argument mismatch
// (`expected &String, found &str`) rather than as a missing-impl error, "because `Q` collapses to
// the key type before any bound is checked".
//
// The sibling fixture writes `cache.get::<str>("a")` to force the unsatisfied bound into view so
// the `#[diagnostic::on_unimplemented]` text can be pinned. No caller writes that turbofish; the
// call a caller actually writes is the one below, and it produces a completely different
// diagnostic. That difference rests on rustc resolving the one `ShardHasher` obligation available
// in the param env (`H: ShardHasher<String>`, hence `Q = String`) before it type-checks the
// argument, so this golden is also what would surface a change in that opportunistic selection.
//
// The `String`/`u32` store types are concrete so the inherent `get`'s `K: Hash + Eq + Clone` and
// `V: Clone` bounds are all satisfied here: the only thing wrong with this helper is the missing
// `ShardHasher<str>`, and E0599 (see `sharded_helper_missing_key_bounds.rs`) cannot pre-empt it.
use cached::{ShardHasher, ShardedUnboundCache};

fn borrowed_lookup<H: ShardHasher<String>>(
    cache: &ShardedUnboundCache<String, u32, H>,
) -> Option<u32> {
    cache.get("a")
}

fn main() {
    let cache: ShardedUnboundCache<String, u32> =
        ShardedUnboundCache::builder().build().expect("build");
    cache.set("a".to_string(), 1);
    let _ = borrowed_lookup(&cache);
}
