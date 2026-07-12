use cached::macros::concurrent_cached;

// map_error is passed to Result::map_err, so an async closure must be rejected
// at the macro with a pointed message rather than failing the FnOnce bound.
#[concurrent_cached(disk = true, ttl_secs = 60, map_error = async |e| format!("{e:?}"))]
async fn my_fn(k: u32) -> Result<u32, String> {
    Ok(k)
}

fn main() {}
