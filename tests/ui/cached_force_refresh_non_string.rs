use cached::macros::cached;

#[cached(ttl_secs = 60, force_refresh = true)]
fn f(k: i32) -> i32 {
    k
}

fn main() {}
