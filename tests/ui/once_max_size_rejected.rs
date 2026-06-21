use cached::macros::once;

#[once(max_size = 10)]
fn f() -> i32 {
    42
}

fn main() {}
