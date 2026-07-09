// Compile-pass: `force_refresh = true` (bare bool) on `#[once]` is accepted.
// The bare bool form is equivalent to `force_refresh = "{ true }"` and causes
// the single cached value to be unconditionally recomputed on every call.
use cached::macros::once;

#[once(force_refresh = true)]
fn f(x: i32) -> i32 {
    x
}

fn main() {
    let _ = f(1);
}
