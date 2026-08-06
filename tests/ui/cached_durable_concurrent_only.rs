use cached::macros::cached;

// `durable` configures the redb disk-backed concurrent store.
#[cached(durable = true)]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {}
