use cached::macros::once;

#[once(convert = "{ a }")]
fn f(a: i32) -> i32 {
    a
}

fn main() {}
