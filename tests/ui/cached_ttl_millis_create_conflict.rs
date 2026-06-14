use cached::macros::cached;

// `create` fully constructs the store, so `ttl_millis` would be silently
// ignored - the macro must reject it with a specific message rather than
// falling through to the generic "cache types are mutually exclusive" error
// (#149). Without the conflict check the store-type match never reaches the
// `create` arm (because `ttl_millis` sets `has_ttl`) and the TTL is dropped.
#[cached(
    ty = "cached::UnboundCache<i32, i32>",
    ttl_millis = 500,
    create = "{ cached::UnboundCache::new() }"
)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
