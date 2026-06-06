use cached::macros::concurrent_cached;

#[derive(Clone)]
struct Val;
impl cached::Expires for Val {
    fn is_expired(&self) -> bool { false }
}

#[concurrent_cached(expires = true, result_fallback = true)]
fn my_fn(x: u32) -> Result<Val, String> {
    Ok(Val)
}

fn main() {}
