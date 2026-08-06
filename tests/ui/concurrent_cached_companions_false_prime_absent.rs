use cached::macros::concurrent_cached;

#[concurrent_cached(companions = false)]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {
    // `companions = false` suppresses the `{fn}_prime_cache` companion.
    let _ = my_fn_prime_cache(1);
}
