use cached::macros::once;

#[once(refresh = true)]
fn f() -> i32 {
    42
}

fn main() {}
