use cached::macros::concurrent_cached;

// `#[concurrent_cached]` requires a `Result` return. `Option<T>` (and other
// single-generic path types) must fail here with a clear message, not deeper
// inside the generated body.
#[concurrent_cached(map_error = "|e| e", disk = true)]
fn my_fn(k: i32) -> Option<i32> {
    Some(k)
}

fn main() {}
