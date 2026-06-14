use cached::macros::concurrent_cached;

struct S;

// A generic `in_impl` method without `key`/`convert` hits the same generic
// rejection as a generic free function: the generic check runs before any
// `in_impl` handling, so the cache key cannot name the type parameter.
impl S {
    #[concurrent_cached(in_impl = true)]
    fn f<T: std::fmt::Debug>(&self, x: T) -> usize {
        let _ = x;
        0
    }
}

fn main() {}
