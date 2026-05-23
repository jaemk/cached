use cached::macros::once;

#[once(ttl = 60, expires = true)]
fn my_fn() -> String {
    "x".to_string()
}

fn main() {}
