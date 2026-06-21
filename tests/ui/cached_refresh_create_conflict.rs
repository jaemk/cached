use cached::macros::cached;

// `create` fully constructs the store, so `refresh` (which the macro would
// otherwise wire via `refresh_on_hit`) would be silently ignored - the macro
// must reject it with a specific message. Mirrors `#[concurrent_cached]`, whose
// `check_create_conflicts` already flags `refresh`.
#[cached(
    ty = "cached::UnboundCache<i32, i32>",
    refresh = true,
    create = "{ cached::UnboundCache::new() }"
)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
