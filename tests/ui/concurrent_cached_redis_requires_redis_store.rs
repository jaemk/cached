use cached::macros::concurrent_cached;

// `redis = true` on a SYNC function builds a `RedisCache`, which is satisfied by
// the `redis_store` feature alone (every redis runtime feature implies it). The
// macro always emits `cached::__require_redis_feature!{}` and lets the
// declarative guard decide: a no-op with a redis feature on, a `compile_error!`
// naming `redis_store` (and the runtime features, for the async case) with it
// off (0042).
//
// This file is only registered with trybuild when `redis_store` is OFF.
#[concurrent_cached(redis = true, ttl_secs = 1, map_error = r#"|e| format!("{:?}", e)"#)]
fn sync_redis_without_redis_store(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
