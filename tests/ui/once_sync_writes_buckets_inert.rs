use cached::macros::once;

// `sync_writes_buckets` is inert on `#[once]` (buckets only apply to
// `sync_writes = "by_key"`, which `#[once]` does not support).
// Explicitly supplying it must be rejected with a clear error.
#[once(sync_writes_buckets = 4)]
fn my_fn() -> i32 {
    42
}

fn main() {}
