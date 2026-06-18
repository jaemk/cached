use cached::macros::cached;

// `key` parses fine as a type, but `convert` is not a valid brace-delimited
// block (unterminated block), so it surfaces the contextual
// "unable to parse `convert` as a block" error (Tier3).
#[cached(key = "u32", convert = "{ this is not a block ")]
fn my_fn(k: u32) -> u32 {
    k
}

fn main() {}
