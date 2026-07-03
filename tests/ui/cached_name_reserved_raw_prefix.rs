use cached::macros::cached;

// The reserved-`__cached`-prefix check is applied to the STRIPPED (bare) name, so a
// raw identifier whose bare form begins with `__cached` (`r#__cachedfoo` -> `__cachedfoo`)
// must STILL be rejected with the reserved-prefix error. It must not panic (the raw
// name would otherwise flow into `Ident::new_raw`) and must not be silently accepted.
#[cached(name = "r#__cachedfoo")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
