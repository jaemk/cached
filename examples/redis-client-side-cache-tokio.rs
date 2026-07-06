/*
RESP3 client-side caching (the `redis_async_cache` feature), Tokio runtime.
`client_side_caching(true)` makes the redis client keep a local,
invalidation-tracked copy of fetched keys, cutting round-trips for hot keys
while staying consistent via server push invalidation.

Note: client-side caching requires RESP3. The `redis_async_cache` capability
feature is runtime-agnostic (it enables the RESP3 client-side-caching path and is
TLS-agnostic); pair it with a runtime feature. This example uses Tokio; add
`redis_tokio_native_tls` or `redis_tokio_rustls` separately if TLS connectivity is
required.

Start a RESP3-capable redis (redis 6+, e.g. the `redis` image) if not already running:
    docker run --rm --name cached-csc-example -p 6379:6379 -d redis
Run:
    CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:6379 \
        cargo run --example redis-client-side-cache-tokio --features "redis_async_cache,redis_tokio_native_tls,proc_macro"
Cleanup:
    docker rm -f cached-csc-example
*/

use cached::AsyncRedisCache;
use cached::macros::concurrent_cached;
use cached::time::Duration;
use std::io;
use std::io::Write;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with redis cache `{0}`")]
    RedisError(String),
}

// The connection string is read from `CACHED_REDIS_CONNECTION_STRING`.
// `.client_side_caching(true)` upgrades the connection to RESP3 and enables
// the client-side cache; everything else is unchanged.
#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "cached::AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::builder("cached-csc-example")
            .ttl(Duration::from_secs(30))
            .client_side_caching(true)
            .build()
            .await
            .expect("error building client-side-caching redis cache")
    } "##
)]
async fn cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(secs.to_string())
}

#[tokio::main]
async fn main() {
    print!("1. first call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).await.unwrap();
    println!("done");
    print!("second call (served via client-side cache, should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).await.unwrap();
    println!("done");

    cached_sleep_secs_prime_cache(5).await.unwrap();
    print!("2. primed call for secs=5 (should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(5).await.unwrap();
    println!("done");
}
