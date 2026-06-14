use cached::macros::once;

#[once(ttl = "core::time::Duration::from_secs(60)", expires = true)]
fn my_fn() -> String {
    "x".to_string()
}

fn main() {}
