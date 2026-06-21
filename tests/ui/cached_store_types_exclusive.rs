use cached::macros::cached;

// `max_size` + `ty` (without `create`) falls into the catch-all arm
// because it does not match any of the recognized store-type combinations.
#[cached(max_size = 5, ty = "cached::UnboundCache<i32, i32>")]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
