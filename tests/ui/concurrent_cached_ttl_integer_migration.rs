use cached::macros::concurrent_cached;

#[concurrent_cached(ttl = 60)]
fn f(x: i32) -> Result<i32, String> {
    Ok(x)
}

fn main() {}
