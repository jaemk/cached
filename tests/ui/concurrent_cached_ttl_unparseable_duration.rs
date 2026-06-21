use cached::macros::concurrent_cached;

#[concurrent_cached(ttl = "core::time::Duration::from_secs(")]
fn f() -> u32 {
    0
}

fn main() {}
