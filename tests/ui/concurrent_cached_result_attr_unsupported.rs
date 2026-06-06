use cached::macros::concurrent_cached;

#[concurrent_cached(result = true)]
fn load(id: u64) -> Result<u64, String> {
    Ok(id)
}

fn main() {}
