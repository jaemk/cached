use cached::macros::once;

#[once(ty = "cached::UnboundCache<(), i32>")]
fn f() -> i32 {
    42
}

fn main() {}
