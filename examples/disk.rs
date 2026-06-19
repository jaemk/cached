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

// When the macro constructs your RedbCache instance (the default disk engine),
// the default cache files will be stored
// under $system_cache_dir/<exe>_cached_disk_cache/
//
// `map_error` is an unquoted closure here; the legacy quoted-string form
// (`map_error = r##"|e| ..."##`) is still accepted.
#[concurrent_cached(
    disk = true,
    ttl_secs = 30,
    map_error = |e| ExampleError::DiskError(format!("{e:?}"))
)]
fn cached_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

// `map_error` is now optional: when the function's error type implements
// `From<RedbCacheError>`, the macro converts store errors automatically via
// `Into::into`, so no `map_error` closure is needed.
impl From<cached::RedbCacheError> for ExampleError {
    fn from(e: cached::RedbCacheError) -> Self {
        ExampleError::DiskError(format!("{e:?}"))
    }
}

#[concurrent_cached(disk = true, ttl_secs = 30)]
fn cached_sleep_secs_from(secs: u64) -> Result<(), ExampleError> {
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
    let cache: RedbCache<u64, u64> = RedbCache::builder()
        .name("disk-example-flush")
        .durable(false)
        .build()
        .unwrap();
    for i in 0..3 {
        cache.set(i, i * 10).unwrap();
    }
    cache.flush().unwrap(); // one durable commit persisting the cheap writes above
    println!("flushed 3 cheap writes to disk in a single durable commit");

    // No `map_error` needed: ExampleError: From<RedbCacheError> handles conversion.
    print!("call without map_error (From<RedbCacheError>) ...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_from(1).unwrap();
    println!("done");
}
