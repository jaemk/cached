use cached::macros::concurrent_cached;

// `create` fully constructs the store, so `refresh` (which the macro would
// otherwise wire via `refresh_on_hit`) would be silently ignored - the macro
// must reject it with a specific message. Parity with the `#[cached]`
// `cached_refresh_create_conflict` fixture.
#[concurrent_cached(
    map_error = "|e| e",
    ty = "cached::UnboundCache<i32, i32>",
    refresh = true,
    create = "{ cached::UnboundCache::new() }"
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
