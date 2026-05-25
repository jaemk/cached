use cached::macros::cached;
use cached::Expires;

#[derive(Clone)]
struct MyVal;
impl Expires for MyVal {
    fn is_expired(&self) -> bool {
        false
    }
}

#[cached(expires = true, unsync_reads = true)]
fn my_fn(x: u32) -> MyVal {
    MyVal
}

fn main() {}
