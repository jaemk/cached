use cached::macros::concurrent_cached;

// map_error must be a closure expression; a plain integer literal must error.
#[concurrent_cached(disk = true, ttl_secs = 60, map_error = 5)]
async fn my_fn(k: u32) -> Result<u32, String> {
    Ok(k)
}

fn main() {}
