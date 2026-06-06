use cached::macros::concurrent_cached;

// `cache_none = true` requires the function to return `Option<T>`.
#[concurrent_cached(cache_none = true)]
fn load(id: u64) -> u64 {
    id
}

fn main() {}
