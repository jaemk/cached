/*
Have Redis or run it locally via docker with the following command: `docker run --rm --name cached-tests -p 6379:6379 -d redis`
Set the required env variable and run this example. Similar to the following command:
`REDIS_CS=redis://127.0.0.1/ cargo run --example redis --features=redis`
 */

#[macro_use]
extern crate cached;

use cached::proc_macro::cached;
use cached::RedisCache;
use std::time::Duration;

fn sleep_secs(secs: u64) {
    std::thread::sleep(Duration::from_secs(secs));
}

cached! {
    SLOW_FN: RedisCache<u64, ()> = RedisCache::with_lifespan(30);
    fn cached_sleep_secs(secs: u64) -> () = {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

async fn async_sleep_secs(secs: u64) {
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

cached! {
    SLOW_ASYNC_FN: RedisCache<u64, ()> = RedisCache::with_lifespan(30);
    async fn cached_async_sleep_secs(secs: u64) -> () = {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }
}

#[tokio::main]
async fn main() {
    if cfg!(feature = "redis") {
        println!("sleeping for 2 seconds");

        async_sleep_secs(2).await;
        println!("first cached sleeping for 2 seconds");
        cached_async_sleep_secs(2).await;
        println!("second cached sleeping for 2 seconds");
        cached_async_sleep_secs(2).await;

        println!("sleeping for 2 seconds");
        sleep_secs(2);
        println!("first cached sleeping for 2 seconds");
        cached_sleep_secs(2);
        println!("second cached sleeping for 2 seconds");
        cached_sleep_secs(2);
    }
}
