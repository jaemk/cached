use cached::macros::concurrent_cached;

mod other {
    pub struct Return<T> {
        pub value: T,
    }
}

#[concurrent_cached(map_error = "|e| e", disk = true, with_cached_flag = true)]
fn my_fn(k: i32) -> Result<other::Return<i32>, String> {
    Ok(other::Return { value: k })
}

fn main() {}
