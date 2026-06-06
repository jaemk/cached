use cached::macros::once;

#[once(cache_none = true)]
fn my_fn() -> i32 {
    0
}

fn main() {}
