use cached::macros::cached;

#[derive(Clone)]
struct Val;
impl cached::Expires for Val {
    fn is_expired(&self) -> bool { false }
}

#[cached(expires = true, cache_err = true)]
fn my_fn(x: u32) -> Result<Val, String> {
    Ok(Val)
}

fn main() {}
