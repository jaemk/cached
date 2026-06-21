use cached::macros::cached;

// A generic `#[cached]` free function without `key`/`convert` hits the generic
// rejection: the cache is a single monomorphic static and cannot name the
// function's type parameter, so the default-key path cannot compile.
#[cached]
fn f<T: std::fmt::Debug>(x: T) -> usize {
    let _ = x;
    0
}

fn main() {}
