use cached::macros::concurrent_cached;

// A `name` beginning with `__cached` is reserved for macro-generated bindings
// and must be rejected.
#[concurrent_cached(name = "__cached_foo")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
