use cached::macros::concurrent_cached;

// Option<T> return type is only supported for the default in-memory sharded path, not redis.
#[concurrent_cached(map_error = "|e| e", redis = true, ttl = 60)]
fn my_fn(k: i32) -> Option<i32> {
    Some(k)
}

fn main() {}
