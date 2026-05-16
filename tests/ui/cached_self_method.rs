use cached::macros::cached;

#[cached]
fn my_fn(&self, k: i32) -> i32 {
    k
}

fn main() {}
