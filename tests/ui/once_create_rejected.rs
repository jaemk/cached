use cached::macros::once;

#[once(create = "{ cached::UnboundCache::new() }")]
fn f() -> i32 {
    42
}

fn main() {}
