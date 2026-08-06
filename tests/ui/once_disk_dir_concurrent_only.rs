use cached::macros::once;

// `disk_dir` sets the redb database directory on the concurrent disk store.
#[once(disk_dir = "/tmp/cached-ui")]
fn my_fn(x: u32) -> u32 {
    x * 2
}

fn main() {}
