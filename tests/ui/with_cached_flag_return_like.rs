use cached::macros::cached;

struct ReturnLike<T> {
    value: T,
}

#[cached(with_cached_flag = true)]
fn my_fn(k: i32) -> ReturnLike<i32> {
    ReturnLike { value: k }
}

fn main() {}
