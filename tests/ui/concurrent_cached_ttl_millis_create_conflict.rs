use cached::macros::concurrent_cached;

// `create` fully constructs the store, so `ttl_millis` would be silently
// ignored - the macro must reject it just as it rejects `ttl` (#149). Without
// the conflict check this compiles and the TTL is dropped without warning.
#[concurrent_cached(
    map_error = "|e| e",
    disk = true,
    ttl_millis = 500,
    create = "{ todo!() }"
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
