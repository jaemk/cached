use cached::macros::concurrent_cached;

// `map_error` must not be accepted when the store is infallible (default sharded in-memory).
#[concurrent_cached(map_error = "|e| e")]
fn simple(n: u64) -> u64 {
    n
}

fn main() {}
