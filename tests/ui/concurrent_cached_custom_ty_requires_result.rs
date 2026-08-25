// A custom `ty` always takes the fallible path, so the cached function must
// return `Result<T, E>`. A plain return type is rejected, spanned at the return
// type rather than at the attribute.
use cached::macros::concurrent_cached;

#[concurrent_cached(
    ty = "cached::ShardedUnboundCache<u64, String>",
    create = "{ cached::ShardedUnboundCache::new() }"
)]
fn my_fn(n: u64) -> String {
    format!("v{n}")
}

fn main() {}
