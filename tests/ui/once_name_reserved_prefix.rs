use cached::macros::once;

// A `name` beginning with `__cached` is reserved for macro-generated bindings
// and must be rejected.
#[once(name = "__cached_foo")]
fn f() -> i32 {
    42
}

fn main() {}
