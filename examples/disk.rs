/*
Synchronous on-disk cache: `#[concurrent_cached(disk = true)]` backed by `redb`.
Default cache files live under $system_cache_dir/<exe>_cached_disk_cache/.

Run:
    cargo run --example disk --features "disk_store,proc_macro"
*/

use cached::macros::concurrent_cached;
use cached::time::Duration;
use std::io;
use std::io::Write;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with disk cache `{0}`")]
    DiskError(String),
}

// When the macro constructs your RedbCache instance (the default disk engine;
// `DiskCache` is a kept type alias), the default cache files will be stored
// under $system_cache_dir/<exe>_cached_disk_cache/
#[concurrent_cached(
    disk = true,
    ttl_secs = 30,
    map_error = r##"|e| ExampleError::DiskError(format!("{:?}", e))"##
)]
fn cached_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

fn main() {
    print!("1. first sync call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");
    print!("second sync call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");

    use cached::ConcurrentCached;
    CACHED_SLEEP_SECS.remove(&2).unwrap();
    print!("third sync call with a 2 seconds sleep (slow, after cache-remove)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");

    // Cheap-writes-then-flush pattern: build a cache with
    // `durable(false)` so each write commits with
    // `Durability::None` (no per-write fsync). This trades per-write durability
    // for write throughput; call `flush()` at a chosen point (periodically or
    // before shutdown) to force a single durable commit that persists them all.
    use cached::RedbCache;
    let cache: RedbCache<u64, u64> = RedbCache::builder("disk-example-flush")
        .durable(false)
        .build()
        .unwrap();
    for i in 0..3 {
        cache.set(i, i * 10).unwrap();
    }
    cache.flush().unwrap(); // one durable commit persisting the cheap writes above
    println!("flushed 3 cheap writes to disk in a single durable commit");
}
