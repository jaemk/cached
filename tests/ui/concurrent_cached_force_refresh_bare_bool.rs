// Compile-pass: `force_refresh = true` (bare bool) on `#[concurrent_cached]` is accepted.
// The bare bool form is equivalent to `force_refresh = "{ true }"` and causes
// the cached value to be unconditionally recomputed on every call.
use cached::macros::concurrent_cached;

#[concurrent_cached(force_refresh = true)]
fn f(x: i32) -> Result<i32, ()> {
    Ok(x)
}

fn main() {
    let _ = f(1);
}
