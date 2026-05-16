use cached::macros::cached;

#[cached(convert = r#"{ k }"#)]
fn my_fn(k: i32) -> i32 {
    k
}

fn main() {}
