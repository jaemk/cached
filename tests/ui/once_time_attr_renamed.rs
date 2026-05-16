use cached::macros::once;

#[once(time = 1)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
