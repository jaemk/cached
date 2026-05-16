/*
Synchronous Redis cache: `#[concurrent_cached(redis = true)]` and an explicit
`ty = "RedisCache<..>"` + `create` store. The connection string is read from
`CACHED_REDIS_CONNECTION_STRING`. (`main` is `#[tokio::main]`, hence the
`async_tokio_rt_multi_thread` feature even though the cache itself is sync.)

Start redis if you don't already have one:
    docker run --rm --name cached-redis-example -p 6379:6379 -d redis
Run:
    CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:6379 \
        cargo run --example redis --features "redis_store,async_tokio_rt_multi_thread,proc_macro"
Cleanup:
    docker rm -f cached-redis-example
*/

use cached::macros::concurrent_cached;
use cached::time::Duration;
use cached::RedisCache;
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
// will be pulled from the env var: `CACHED_REDIS_CONNECTION_STRING`;
#[concurrent_cached(
    redis = true,
    ttl = 30,
    cache_prefix_block = r##"{ "cache-redis-example-1" }"##,
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##
)]
fn cached_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

// If not `cache_prefix_block` is specified, then the function name
// is used to create a prefix for cache keys used by this function
#[concurrent_cached(
    redis = true,
    ttl = 30,
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##
)]
fn cached_sleep_secs_example_2(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
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
    ty = "cached::RedisCache<u64, String>",
    create = r##" {
        RedisCache::new("cache_redis_example_cached_sleep_secs_config", Duration::from_secs(1))
            .refresh(true)
            .connection_string(&CONFIG.conn_str)
            .build()
            .expect("error building example redis cache")
    } "##
)]
fn cached_sleep_secs_config(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(secs.to_string())
}

#[tokio::main]
async fn main() {
    print!("1. first sync call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");
    print!("second sync call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");

    use cached::ConcurrentCached;
    CACHED_SLEEP_SECS.cache_remove(&2).unwrap();
    print!("third sync call with a 2 seconds sleep (slow, after cache-remove)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");

    print!("2. first sync call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_example_2(2).unwrap();
    println!("done");
    print!("second sync call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_example_2(2).unwrap();
    println!("done");

    cached_sleep_secs_config_prime_cache(2).unwrap();
    print!("3. first primed async call with a 2 seconds sleep (should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_config(2).unwrap();
    println!("done");
    print!("second async call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_config(2).unwrap();
    println!("done");
}
