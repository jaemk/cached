/*!
Coverage for the release shell scripts in `bin/`.

Neither script is exercised by a normal build, and both only run during a
release, where a mistake is expensive and hard to reverse: `check-versions.sh`
is the guard that stops a half-bumped workspace from publishing, and
`publish.sh`'s exit status decides whether a release reports green.

The `check-versions.sh` failure cases are driven by handing the script a crafted
`cargo metadata` document, which is why it accepts one as an argument.
*/

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Scratch files live in the repo's gitignored `local/` directory, matching the
/// other tests that need real files on disk.
fn scratch_dir() -> TempDir {
    let root = repo_root().join("local");
    std::fs::create_dir_all(&root).expect("create local/ scratch root");
    TempDir::new_in(root).expect("create scratch dir")
}

fn run(script: &str, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(repo_root().join("bin").join(script))
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("failed to run bin/{script}: {e}"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A `cargo metadata --no-deps` document with just the fields the script reads.
fn metadata(root_version: &str, pins: &[(&str, &str)], packages: &[(&str, &str)]) -> String {
    let deps: Vec<String> = pins
        .iter()
        .map(|(name, req)| format!(r#"{{"name":"{name}","req":"{req}"}}"#))
        .collect();
    let mut entries = vec![format!(
        r#"{{"name":"cached","version":"{root_version}","dependencies":[{}]}}"#,
        deps.join(",")
    )];
    for (name, version) in packages {
        entries.push(format!(
            r#"{{"name":"{name}","version":"{version}","dependencies":[]}}"#
        ));
    }
    format!(r#"{{"packages":[{}]}}"#, entries.join(","))
}

fn write_metadata(dir: &TempDir, contents: &str) -> PathBuf {
    let path = dir.path().join("metadata.json");
    std::fs::write(&path, contents).expect("write metadata fixture");
    path
}

fn check_versions_with(dir: &TempDir, contents: &str) -> Output {
    let path = write_metadata(dir, contents);
    run("check-versions.sh", &[path.to_str().expect("utf-8 path")])
}

/// The real workspace must always satisfy its own release guard. This is the
/// test that fails if someone bumps the root crate without bumping the
/// `cached_proc_macro*` pins (or vice versa) -- the exact half-bumped state the
/// release workflow cannot otherwise see.
#[test]
fn the_committed_workspace_versions_agree() {
    let out = run("check-versions.sh", &[]);
    assert!(
        out.status.success(),
        "bin/check-versions.sh must pass against the committed workspace:\n{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("Workspace versions agree"));
}

/// Root bumped, pins left behind: `cargo publish` would resolve the stale pins
/// against the index and ship a stable `cached` depending on the old subcrates.
#[test]
fn a_stale_dependency_pin_fails_the_check() {
    let dir = scratch_dir();
    let out = check_versions_with(
        &dir,
        &metadata(
            "3.0.0",
            &[
                ("cached_proc_macro", "^3.0.0-rc.10"),
                ("cached_proc_macro_types", "^3.0.0"),
            ],
            &[
                ("cached_proc_macro", "3.0.0"),
                ("cached_proc_macro_types", "3.0.0"),
            ],
        ),
    );

    assert!(!out.status.success(), "a stale pin must fail the check");
    let err = stderr(&out);
    assert!(
        err.contains("cached depends on cached_proc_macro ^3.0.0-rc.10"),
        "the error must name both versions:\n{err}"
    );
    assert!(err.contains("refusing to publish"));
}

/// Pins bumped, subcrate package versions left behind: the subcrate publish is
/// skipped as already-published and the root then requires a version that does
/// not exist on the index.
#[test]
fn an_unbumped_subcrate_package_version_fails_the_check() {
    let dir = scratch_dir();
    let out = check_versions_with(
        &dir,
        &metadata(
            "3.0.0",
            &[
                ("cached_proc_macro", "^3.0.0"),
                ("cached_proc_macro_types", "^3.0.0"),
            ],
            &[
                ("cached_proc_macro", "3.0.0-rc.10"),
                ("cached_proc_macro_types", "3.0.0"),
            ],
        ),
    );

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the workspace builds cached_proc_macro 3.0.0-rc.10"),
        "the error must name the workspace version:\n{}",
        stderr(&out)
    );
}

/// Internally consistent, but a stable release must not depend on a
/// pre-release: that pins every downstream user to a pre-release resolution.
#[test]
fn a_stable_root_depending_on_a_prerelease_subcrate_fails_the_check() {
    let dir = scratch_dir();
    let out = check_versions_with(
        &dir,
        &metadata(
            "3.0.0",
            &[
                ("cached_proc_macro", "^3.0.0-rc.10"),
                ("cached_proc_macro_types", "^3.0.0"),
            ],
            &[
                ("cached_proc_macro", "3.0.0-rc.10"),
                ("cached_proc_macro_types", "3.0.0"),
            ],
        ),
    );

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("Pre-release dependency"),
        "the pre-release case must be reported as such:\n{}",
        stderr(&out)
    );
}

/// A pre-release root legitimately depends on pre-release subcrates; that is
/// every release candidate this crate has shipped.
#[test]
fn a_prerelease_root_may_depend_on_prerelease_subcrates() {
    let dir = scratch_dir();
    let out = check_versions_with(
        &dir,
        &metadata(
            "3.0.0-rc.10",
            &[
                ("cached_proc_macro", "^3.0.0-rc.10"),
                ("cached_proc_macro_types", "^3.0.0-rc.10"),
            ],
            &[
                ("cached_proc_macro", "3.0.0-rc.10"),
                ("cached_proc_macro_types", "3.0.0-rc.10"),
            ],
        ),
    );

    assert!(
        out.status.success(),
        "a pre-release root must be allowed pre-release subcrates:\n{}",
        stderr(&out)
    );
}

/// Put a fake `cargo` at the front of `PATH` that prints `output` and exits
/// with `code`, then run `publish.sh` against it. `publish.sh` shells out to a
/// bare `cargo publish`, so this substitutes for every crate it tries.
fn publish_with_stub_cargo(dir: &TempDir, output: &str, code: i32) -> Output {
    let bin = dir.path().join("stub-bin");
    std::fs::create_dir_all(&bin).expect("create stub bin dir");
    let stub = bin.join("cargo");
    std::fs::write(
        &stub,
        format!("#!/bin/bash\ncat <<'STUB_EOF'\n{output}\nSTUB_EOF\nexit {code}\n"),
    )
    .expect("write stub cargo");

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("make stub cargo executable");

    let path = std::env::var("PATH").unwrap_or_default();
    Command::new("bash")
        .arg(repo_root().join("bin").join("publish.sh"))
        .current_dir(repo_root())
        .env("PATH", format!("{}:{path}", bin.display()))
        .output()
        .expect("failed to run bin/publish.sh")
}

/// Cargo's pre-flight check against the index produces this wording.
#[test]
fn publish_treats_an_index_conflict_as_a_benign_skip() {
    let dir = scratch_dir();
    let out = publish_with_stub_cargo(
        &dir,
        "error: crate version `3.0.0` already exists on crates.io index",
        1,
    );

    assert!(
        out.status.success(),
        "an already-published version must not fail the release:\n{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("3 already current"));
}

/// crates.io rejects the upload itself with this wording, which is what a re-run
/// after a partial publish hits. Matching only the pre-flight message made the
/// documented re-run guarantee fail hard on exactly that case.
#[test]
fn publish_treats_a_server_side_upload_conflict_as_a_benign_skip() {
    let dir = scratch_dir();
    let out = publish_with_stub_cargo(
        &dir,
        "error: failed to publish to registry at https://crates.io\n\nCaused by:\n  the remote server responded with an error: crate version `3.0.0` is already uploaded",
        1,
    );

    assert!(
        out.status.success(),
        "a server-side already-uploaded rejection must be a skip, not a failure:\n{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("3 already current"));
}

/// Every other failure still fails the release. This is the property the
/// already-published carve-outs must not erode.
#[test]
fn publish_still_fails_the_release_on_any_other_error() {
    let dir = scratch_dir();
    let out = publish_with_stub_cargo(&dir, "error: failed to verify package tarball", 1);

    assert!(
        !out.status.success(),
        "an unrelated publish failure must fail the release"
    );
    assert!(stdout(&out).contains("3 failed"));
    assert!(stderr(&out).contains("Release failed"));
}
