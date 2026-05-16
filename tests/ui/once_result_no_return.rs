use cached::macros::once;

#[once(result = true)]
fn my_fn(k: i32) {
    let _ = k;
}

fn main() {}
