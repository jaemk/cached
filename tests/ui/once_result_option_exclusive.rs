use cached::macros::once;

#[once(result = true, option = true)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {}
