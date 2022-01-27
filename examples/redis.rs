/*
Have Redis or run it locally via docker with the following command: `docker run --rm --name cached-tests -p 6379:6379 -d redis`
Set the required env variable and run this example. Similar to the following command:
`REDIS_CS=redis://127.0.0.1/ cargo run --example redis --features=redis`
 */

#[macro_use]
extern crate cached;

use cached::proc_macro::cached;
use cached::RedisCache;
use std::io;
use std::io::Write;
use std::time::Duration;

cached! {
    SLOW_FN: RedisCache<u64, ()> = RedisCache::with_lifespan(30);
    fn cached_sleep_secs(secs: u64) -> () = {
        std::thread::sleep(Duration::from_secs(secs));
    }
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
        print!("first async call with a 2 seconds sleep...");
        io::stdout().flush().unwrap();
        cached_async_sleep_secs(2).await;
        println!("done");
        print!("second async call with a 2 seconds sleep (it should be fast)...");
        io::stdout().flush().unwrap();
        cached_async_sleep_secs(2).await;
        println!("done");

        print!("first sync call with a 2 seconds sleep...");
        io::stdout().flush().unwrap();
        cached_sleep_secs(2);
        println!("done");
        print!("second sync call with a 2 seconds sleep (it should be fast)...");
        io::stdout().flush().unwrap();
        cached_sleep_secs(2);
        println!("done");
    }
}
