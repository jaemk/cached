use cached::macros::once;

#[once(unsync_reads = true)]
fn f() -> i32 {
    42
}

fn main() {}
