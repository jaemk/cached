use cached::macros::once;

#[once(expires = true)]
fn my_fn() -> String {
    "hello".to_string()
}

fn main() {}
