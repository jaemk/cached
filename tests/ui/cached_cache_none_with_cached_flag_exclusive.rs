use cached::macros::cached;

#[cached(cache_none = true, with_cached_flag = true)]
fn my_fn(k: i32) -> Option<cached::Return<i32>> {
    Some(cached::Return::new(k))
}

fn main() {}
