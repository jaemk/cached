use cached::macros::cached;

#[derive(Clone)]
struct Val;
impl cached::Expires for Val {
    fn is_expired(&self) -> bool { false }
}

#[cached(expires = true, cache_none = true)]
fn my_fn(x: u32) -> Option<Val> {
    Some(Val)
}

fn main() {}
