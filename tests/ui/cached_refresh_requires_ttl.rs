use cached::macros::cached;

#[cached(refresh = true)]
fn f(k: i32) -> i32 {
    k
}

fn main() {}
