/*
Async Redis cache (`AsyncRedisCache`, async-std runtime): `#[concurrent_cached]`
with a custom `create` block and cache priming. Uses `redis_smol` for the Redis
connection (smol is the async executor that async-std is built on). The
connection string is read from `CACHED_REDIS_CONNECTION_STRING`.

See also `redis-async-tokio` for the same example on the Tokio runtime.

Note: `redis_smol` enables TLS via native-tls. If you need plain-text connections
only, you can enable `redis/smol-comp` directly instead.

Note: `redis_smol` transitively enables `tokio` (through cached's `async`
feature) so that cached's internal sync primitives compile, but the Tokio
runtime is never started — all async execution is driven by async-std.

Start redis if you don't already have one:
    docker run --rm --name async-cached-redis-example -p 6379:6379 -d redis
Run:
    CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:6379 \
        cargo run --example redis-async-async-std --features "redis_smol,proc_macro"
Cleanup:
    docker rm -f async-cached-redis-example
*/

use async_std::task::sleep;
use cached::AsyncRedisCache;
use cached::macros::concurrent_cached;
use cached::time::Duration;
use std::io;
use std::io::Write;
use std::sync::LazyLock;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with redis cache `{0}`")]
    RedisError(String),
}

// When the macro constructs your RedisCache instance, the connection string
// will be pulled from the env var: `CACHED_REDIS_CONNECTION_STRING`.
#[concurrent_cached(
    redis = true,
    ttl = 30,
    cache_prefix_block = r##"{ "cache-redis-async-std-example-1" }"##,
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##
)]
async fn cached_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    sleep(Duration::from_secs(secs)).await;
    Ok(())
}

#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "cached::AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::new("cache_redis_async_std_example_cached_sleep_secs", Duration::from_secs(1))
            .refresh_on_hit(true)
            .build()
            .await
            .expect("error building example redis cache")
    } "##
)]
async fn async_cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    sleep(Duration::from_secs(secs)).await;
    Ok(secs.to_string())
}

struct Config {
    conn_str: String,
}
impl Config {
    fn load() -> Self {
        Self {
            conn_str: std::env::var("CACHED_REDIS_CONNECTION_STRING").unwrap(),
        }
    }
}

static CONFIG: LazyLock<Config> = LazyLock::new(Config::load);

#[concurrent_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    ty = "cached::AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::new("cache_redis_async_std_example_cached_sleep_secs_config", Duration::from_secs(1))
            .refresh_on_hit(true)
            .connection_string(&CONFIG.conn_str)
            .build()
            .await
            .expect("error building example redis cache")
    } "##
)]
async fn async_cached_sleep_secs_config(secs: u64) -> Result<String, ExampleError> {
    sleep(Duration::from_secs(secs)).await;
    Ok(secs.to_string())
}

#[async_std::main]
async fn main() {
    print!("1. first call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).await.unwrap();
    println!("done");
    print!("second call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).await.unwrap();
    println!("done");

    print!("2. first async call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs(2).await.unwrap();
    println!("done");
    print!("second async call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs(2).await.unwrap();
    println!("done");

    async_cached_sleep_secs_config_prime_cache(2).await.unwrap();
    print!("3. first primed async call with a 2 seconds sleep (should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs_config(2).await.unwrap();
    println!("done");
    print!("second async call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs_config(2).await.unwrap();
    println!("done");
}
