use cached::once;

#[once(ttl_secs = 0)]
fn my_fn() -> i32 {
    42
}

fn main() {}
