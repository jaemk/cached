use cached::macros::once;

#[once]
fn my_fn(&self, k: i32) -> i32 {
    k
}

fn main() {}
