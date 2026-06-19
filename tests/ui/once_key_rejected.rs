use cached::macros::once;

#[once(key = "i32", convert = "{ a }")]
fn f(a: i32) -> i32 {
    a
}

fn main() {}
