use cached::macros::cached;

#[cached(sync_lock = "spinlock")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
