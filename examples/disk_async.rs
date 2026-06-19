/*
Async disk cache. `redb` has no async API, so `#[concurrent_cached(disk = true)]`
on an `async fn` runs the blocking I/O on a background thread via the `blocking`
crate -- it never stalls the async runtime and works with any executor.

Run:
    cargo run --example disk_async --features "disk_store,async,proc_macro"
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

// Distinct cache name so this example's on-disk store does not collide with the
// synchronous `disk` example (default files under
// $system_cache_dir/<exe>_cached_disk_cache/).
#[concurrent_cached(
    disk = true,
    ttl_secs = 30,
    name = "ASYNC_DISK_SLEEP_SECS",
    map_error = r##"|e| ExampleError::DiskError(format!("{:?}", e))"##
)]
async fn async_disk_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

#[tokio::main]
async fn main() {
    print!("1. first async call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    async_disk_sleep_secs(2).await.unwrap();
    println!("done");
    print!("second async call (served from disk cache, should be fast)...");
    io::stdout().flush().unwrap();
    async_disk_sleep_secs(2).await.unwrap();
    println!("done");

    print!("2. prime the cache for secs=5, then call (should be fast)...");
    io::stdout().flush().unwrap();
    async_disk_sleep_secs_prime_cache(5).await.unwrap();
    async_disk_sleep_secs(5).await.unwrap();
    println!("done");
}
