use cached::macros::concurrent_cached;

// `option = true` was never a valid attribute for `#[concurrent_cached]`.
#[concurrent_cached(option = true)]
fn find(id: u64) -> Option<u64> {
    Some(id)
}

fn main() {}
