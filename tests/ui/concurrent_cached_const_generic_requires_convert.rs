use cached::macros::concurrent_cached;

// A const-generic `#[concurrent_cached]` function without `key`/`convert` hits
// the generic rejection: the cache is a single monomorphic static and cannot
// name the function's const parameter, so the default-key path cannot compile.
#[concurrent_cached]
fn f<const N: usize>(x: i32) -> i32 {
    let _ = N;
    x
}

fn main() {}
