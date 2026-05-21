use cached::macros::concurrent_cached;

// `create` fully constructs the store, so `disk_dir` would be silently
// ignored — the macro must reject it instead of dropping a path/durability
// setting the user believes is applied.
#[concurrent_cached(
    map_error = "|e| e",
    disk = true,
    disk_dir = "/tmp/x",
    create = "{ todo!() }"
)]
fn my_fn(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
