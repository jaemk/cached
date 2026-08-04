use cached::macros::concurrent_cached;

// `redis = true` on an ASYNC function builds an `AsyncRedisCache`, which needs a
// redis *runtime* feature (`redis_tokio` / `redis_smol` or one of their TLS
// variants) - `redis_store` alone is not enough. The macro emits the `{async}`
// arm, `cached::__require_redis_feature!{async}` (0042).
//
// The point of this fixture: the error must name the redis runtime features and
// must NOT name `async` / `async_core`. Before the guard existed, this program
// reached the async path and reported the doc-hidden internal
// `cached::__set_dispatch_async` with a rustc note pointing at `async_core` - a
// real `cached` feature, which makes it credible, and the WRONG one: enabling
// `async_core` does not fix the build, while `redis_tokio` / `redis_smol` do
// (each implies `redis_store` AND `async`).
//
// This file is registered with trybuild only when `async` is ON and no redis
// feature is enabled, which is exactly the configuration that isolates the redis
// guard: the `async` guard is a no-op, so the redis `compile_error!` is the only
// diagnostic. `tests/ui/concurrent_cached_async_redis_guard_order.rs` covers the
// complementary case (`async` off), where the guard ORDER is what matters.
#[concurrent_cached(redis = true, ttl_secs = 1, map_error = r#"|e| format!("{:?}", e)"#)]
async fn async_redis_without_redis_runtime(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
