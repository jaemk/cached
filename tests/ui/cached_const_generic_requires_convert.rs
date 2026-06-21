use cached::macros::cached;

// A const-generic `#[cached]` free function without `key`/`convert` hits the
// generic rejection: the cache is a single monomorphic static and cannot name
// the function's const parameter, so the default-key path cannot compile.
#[cached]
fn f<const N: usize>(x: i32) -> i32 {
    let _ = N;
    x
}

fn main() {}
