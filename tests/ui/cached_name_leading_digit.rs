// `name` must be a valid Rust identifier: a leading digit (`123`) is not an
// identifier and must be rejected with the spanned error, even though it is a
// "word" with no dashes (unlike the `bad-name` fixture).
use cached::macros::cached;

#[cached(name = "123")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
