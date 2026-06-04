use cached::macros::concurrent_cached;

// `Option<T>` returns are supported on the default in-memory path, but not
// with `disk`/`redis`/custom stores. This must fail with a clear message.
#[concurrent_cached(map_error = "|e| e", disk = true)]
fn my_fn(k: i32) -> Option<i32> {
    Some(k)
}

fn main() {}
