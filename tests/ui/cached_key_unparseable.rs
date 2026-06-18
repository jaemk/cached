use cached::macros::cached;

#[cached(key = "not a type !!", convert = "{ k }")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
