use cached::macros::concurrent_cached;

#[concurrent_cached(sync_writes = true)]
fn load(id: u64) -> u64 {
    id
}

fn main() {}
