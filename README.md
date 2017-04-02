# cached

> simple rust caching macro

### wip!

## Usage

Easy to use caching inspired by python decorators.

```rust
#[macro_use] extern crate cached;

cached!{ FIB >>
fib(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib(n-1) + fib(n-2)
}}

pub fn main() {
    fib(20);
    fib(20);
    {
        let cache = FIB.lock().unwrap();
        println!("hits: {:?}", cache.hits());
        println!("misses: {:?}", cache.misses());
        // make sure the cache-lock is dropped
    }
    println!("fib(20) = {}", fib(20));
}
```

