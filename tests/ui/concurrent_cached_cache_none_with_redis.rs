use cached::macros::concurrent_cached;

// Option<T> + cache_none=true on redis should say "Option<T> return types", not "plain".
#[concurrent_cached(map_error = "|e| e", redis = true, ttl_secs = 60, cache_none = true)]
fn my_fn(k: i32) -> Option<i32> {
    Some(k)
}

fn main() {}
