use cached::macros::once;

#[derive(Clone)]
struct Val;
impl cached::Expires for Val {
    fn is_expired(&self) -> bool { false }
}

#[once(expires = true, cache_err = true)]
fn my_fn() -> Result<Val, String> {
    Ok(Val)
}

fn main() {}
