use cached::macros::concurrent_cached;

// `Option<Return<T>>` with `cache_none = true` is not supported alongside `with_cached_flag`.
#[concurrent_cached(with_cached_flag = true, cache_none = true)]
fn my_fn(k: i32) -> Option<cached::Return<i32>> {
    Some(cached::Return::new(k * 2))
}

fn main() {}
