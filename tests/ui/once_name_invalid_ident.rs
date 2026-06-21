use cached::macros::once;

#[once(name = "bad-name")]
fn f() -> i32 {
    42
}

fn main() {}
