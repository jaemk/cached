use cached::once;

#[once(ttl = 0)]
fn my_fn() -> i32 {
    42
}

fn main() {}
