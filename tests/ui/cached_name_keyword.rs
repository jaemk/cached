// `name` must be a valid Rust identifier: a reserved keyword (`fn`) parses as a
// keyword token, not an `Ident`, so it must be rejected with the spanned error
// (and must not slip through to `Ident::new`, which would panic on a keyword).
use cached::macros::cached;

#[cached(name = "fn")]
fn f(x: i32) -> i32 {
    x
}

fn main() {}
