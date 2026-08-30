// Break class 2, the one breaking change 0055 does NOT fix and explicitly must not be claimed to
// fix (`specs/design/0055`, Pitfalls). Making the inherent lookups generic over `Q` at all is what
// causes it, so it survives the `BorrowedKeyRouting` -> `ShardHasher<Q>` route change unchanged.
//
// `CHANGELOG.md`, the crate-root docs and `README.md` all quote this exact diagnostic
// (``the trait bound `String: Borrow<&String>` is not satisfied``) as the thing a 3.1.x caller
// will see, and `tests/sharded_generic_helper_bounds.rs::lookup_all` pins only the WORKING form
// (`cache.get(k)`). This golden pins the failing form, so the documented migration text cannot
// drift from the compiler's actual output, and so a future change that alters the shape of this
// break -- fixing it, or making it fail differently -- is visible rather than silent.
use cached::ShardedLruCache;

fn main() {
    let cache: ShardedLruCache<String, u32> = ShardedLruCache::new(64);
    cache.set("a".to_string(), 1);
    let keys = vec!["a".to_string()];
    for k in &keys {
        // `k: &String` here, so the extra `&` infers `Q = &String` and needs
        // `String: Borrow<&String>`, which does not exist. The documented fix is `cache.get(k)`.
        let _ = cache.get(&k);
    }
}
