use cached::macros::cached;

// Malformed unquoted convert block: `let x =` is not a valid expression,
// so darling rejects it during attribute parsing.
#[cached(key = "u32", convert = { let x = })]
fn my_fn(k: u32) -> u32 {
    k
}

fn main() {}
