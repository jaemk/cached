use cached::macros::once;

#[once(sync_lock = "mutex")]
fn f() -> i32 {
    42
}

fn main() {}
