use cached::macros::concurrent_cached;

// Guard ORDER (0042): on an async `redis = true` function the macro emits
// `cached::__require_redis_feature!{async}` BEFORE
// `cached::__require_async_feature!{}`, so that a build with neither feature
// reports the missing redis runtime feature FIRST. Reporting `async` first is not
// merely less useful: the redis runtime features imply `async`, so `redis_tokio` /
// `redis_smol` is the single thing to enable, and enabling `async` alone leaves
// the build broken.
//
// This file is registered with trybuild only when BOTH `async` and `redis_store`
// are off, which is the only configuration where both guards fire and the
// relative order is observable. The golden therefore pins the sequence; a
// re-ordering of the two emitted guards shows up as a `.stderr` diff.
//
// The trailing `__set_dispatch_async` / `async_core` resolution error in the
// golden is the pre-existing missing-`async` leak, orthogonal to the redis guard:
// it is what the `async` guard (the second error here) is about. The isolated
// redis-only diagnostic - one error, no `async_core` anywhere - is covered by
// `tests/ui/concurrent_cached_async_redis_requires_redis_runtime.rs`.
#[concurrent_cached(redis = true, ttl_secs = 1, map_error = r#"|e| format!("{:?}", e)"#)]
async fn async_redis_without_any_feature(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
