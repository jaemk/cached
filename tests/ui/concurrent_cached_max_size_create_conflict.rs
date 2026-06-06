use cached::macros::concurrent_cached;

// `max_size` is an alias for `size`; the create-conflict diagnostic must name the
// attribute the user actually wrote (`max_size`), not the reconciled `size`.
#[concurrent_cached(map_error = "|e| e", disk = true, create = "{ }", max_size = 8)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
