// Compile-pass: `result_fallback = true` with `sync_writes = false` (Disabled) must compile.
// Only non-Disabled sync_writes values conflict with result_fallback; explicitly
// setting `sync_writes = false` is the same as the default and is compatible.
use cached::macros::cached;

#[cached(ttl_secs = 60, result_fallback = true, sync_writes = false)]
fn my_fn(k: i32) -> Result<i32, ()> {
    Ok(k)
}

fn main() {
    let _ = my_fn(1);
}
