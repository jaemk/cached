use cached::macros::cached;

#[cached(companions = false)]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {
    // `companions = false` suppresses the `{fn}_no_cache` origin companion.
    let _ = my_fn_no_cache(1);
}
