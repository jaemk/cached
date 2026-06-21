use cached::macros::concurrent_cached;

// A generic `#[concurrent_cached]` function without `key`/`convert` hits the
// generic rejection: the cache is a single monomorphic static and cannot name
// the function's type parameter, so the default-key path cannot compile.
#[concurrent_cached]
fn f<T: std::fmt::Debug>(x: T) -> usize {
    let _ = x;
    0
}

fn main() {}
