use cached::macros::once;

#[once(cache_none = true, with_cached_flag = true)]
fn my_fn() -> Option<cached::Return<i32>> {
    Some(cached::Return::new(0))
}

fn main() {}
