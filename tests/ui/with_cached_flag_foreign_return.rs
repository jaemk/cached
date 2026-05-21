use cached::macros::cached;

mod other {
    pub struct Return<T> {
        pub value: T,
    }
}

#[cached(with_cached_flag = true)]
fn my_fn(k: i32) -> other::Return<i32> {
    other::Return { value: k }
}

fn main() {}
