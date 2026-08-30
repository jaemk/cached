// Per specs/design/0055 (Pitfalls): this is the genuinely unsatisfied-bound case, where
// `ShardHasher`'s `#[diagnostic::on_unimplemented]` message DOES fire. A generic helper bounded
// only on `H: ShardHasher<K>` (the bound the store's own `impl` block already carries) cannot
// perform a borrowed-key lookup at a different `Q` without also bounding `H: ShardHasher<Q>`.
// Unlike `sharded_router_missing_borrowed_impl.rs`, `H` here is a genuine, unresolved type
// parameter rather than a concrete router with a single impl for inference to collapse onto, so
// this is a real unsatisfied bound and the diagnostic attribute has something to attach to.
use cached::{ShardHasher, ShardedUnboundCache};

fn borrowed_lookup<H: ShardHasher<String>>(
    cache: &ShardedUnboundCache<String, u32, H>,
) -> Option<u32> {
    // `Q` is pinned explicitly with turbofish so inference cannot silently resolve it back to
    // `String` (the one `Q` this function's bounds happen to cover) the way it does in
    // `sharded_router_missing_borrowed_impl.rs`. With `Q = str` fixed, this requires
    // `H: ShardHasher<str>`, which is not among this function's bounds: a genuine unsatisfied
    // bound, not an inference collapse.
    cache.get::<str>("a")
}

fn main() {
    let cache: ShardedUnboundCache<String, u32> = ShardedUnboundCache::builder()
        .build()
        .expect("build");
    cache.set("a".to_string(), 1);
    let _ = borrowed_lookup(&cache);
}
