use cached::macros::concurrent_cached;

#[concurrent_cached(ttl = "core::time::Duration::from_secs(1)", ttl_millis = 500)]
fn f(x: i32) -> Result<i32, String> {
    Ok(x)
}

fn main() {}
