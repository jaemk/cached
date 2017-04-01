#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use cached::{Cache};


fn fib(n: u32) -> u32 {
    let (mut a, mut b) = (0, 1);
    let mut sum = 0;
    for _ in 0..n {
        sum += a;
        let hold = a;
        a = b;
        b = hold + b;
    }
    sum
}
cached_with!{ CachedFib ; Cache<u32, u32>; fib ; n: u32; u32}



cached!{ FC ; rec_fib(n: u32) -> u32 ; {
    if n == 0 || n == 1 { return n; }
    rec_fib(n-1) + rec_fib(n-2)
}}


pub fn main() {
    let mut f = CachedFib::new(Cache::new());
    let _ = f.call(5);
    let res = f.call(5);
    println!("cached fib: {}", res);

    rec_fib(5);
    rec_fib(5);
    rec_fib(10);
    rec_fib(10);
}
