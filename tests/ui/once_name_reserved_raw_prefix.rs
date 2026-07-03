use cached::macros::once;

// The reserved-`__cached`-prefix check is applied to the STRIPPED (bare) name, so a
// raw identifier whose bare form begins with `__cached` (`r#__cachedfoo` -> `__cachedfoo`)
// must STILL be rejected with the reserved-prefix error, not panic or be accepted.
#[once(name = "r#__cachedfoo")]
fn f() -> i32 {
    42
}

fn main() {}
