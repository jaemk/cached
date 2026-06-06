use cached::macros::once;

#[once(cache_err = true)]
fn my_fn() -> i32 {
    0
}

fn main() {}
