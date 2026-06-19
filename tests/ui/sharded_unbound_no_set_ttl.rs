// Negative surface for the required-trait design (the point of the trait split):
// non-TTL concurrent stores intentionally do NOT implement `ConcurrentCacheTtl`, so
// the global-TTL knobs (`set_ttl`/`ttl`/`unset_ttl`) do not exist on them at all.
// `ShardedUnboundCache` has no global TTL, so `set_ttl` must NOT resolve even with the
// prelude glob (which brings `ConcurrentCacheTtl` into scope). If a future change
// implemented `ConcurrentCacheTtl` for a non-TTL store, this would start compiling and
// the golden would break, flagging the regression.
use cached::prelude::*;
use cached::ShardedUnboundCache;
use std::time::Duration;

fn main() {
    let cache: ShardedUnboundCache<u32, u32> =
        ShardedUnboundCache::builder().build().expect("build");
    // `set_ttl` is on `ConcurrentCacheTtl`, which `ShardedUnboundCache` does not implement.
    let _ = cache.set_ttl(Duration::from_secs(60));
}
