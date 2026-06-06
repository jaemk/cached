use cached::macros::once;

#[derive(Clone)]
struct Val;
impl cached::Expires for Val {
    fn is_expired(&self) -> bool { false }
}

#[once(expires = true, cache_none = true)]
fn my_fn() -> Option<Val> {
    Some(Val)
}

fn main() {}
