#[macro_use] extern crate cached;
// `cached!` macro requires the `lazy_static!` macro
#[macro_use] extern crate lazy_static;

use std::time::{Instant, Duration};
use std::thread::sleep;

use cached::SizedCache;


cached!{ SLOW: SizedCache = SizedCache::with_capacity(50); >>
fn slow(n: u32) -> () = {
    if n == 0 { return; }
    sleep(Duration::new(1, 0));
    slow(n-1)
}}

pub fn main() {
    println!("running fresh...");
    let now = Instant::now();
    slow(10);
    println!("fresh! elapsed: {}", now.elapsed().as_secs());

    println!("running cached...");
    let now = Instant::now();
    slow(10);
    println!("cached!! elapsed: {}", now.elapsed().as_secs());

    {
        use cached::Cached;  // must be in scope to access cache

        println!(" ** Cache info **");
        let cache = SLOW.lock().unwrap();
        println!("hits=1 -> {:?}", cache.cache_hits().unwrap() == 1);
        println!("misses=11 -> {:?}", cache.cache_misses().unwrap() == 11);
        // make sure the cache-lock is dropped
    }
}
