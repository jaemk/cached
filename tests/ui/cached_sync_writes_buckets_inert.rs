use cached::macros::cached;

// `sync_writes_buckets` only applies to `sync_writes = "by_key"`. Supplying it
// with any other `sync_writes` mode (including unset) must be rejected with a
// clear error instead of being silently ignored.
#[cached(sync_writes_buckets = 128)]
fn unset_mode(x: u32) -> u32 {
    x
}

#[cached(sync_writes = true, sync_writes_buckets = 128)]
fn default_mode(x: u32) -> u32 {
    x
}

fn main() {}
