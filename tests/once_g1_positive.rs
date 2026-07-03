/*!
Regression tests for the G1 guard false-rejection bug in `#[once]`.

The substring check `value_ty_str.contains(param_ident)` incorrectly rejected
generic `#[once]` functions whose concrete return type happened to contain a
generic param ident as a substring (e.g. param `S` inside return type `String`,
or param `Res` inside return type `Result<u32, String>`).

These functions MUST compile and run correctly after the fix.
*/

#![cfg(feature = "proc_macro")]

use cached::macros::once;

// --- case 1: type param `S`, concrete return type `String` ---
// "String" contains "S" as a substring; the old check falsely rejected this.
static CALLS_TAKES_S: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[once]
fn takes_s_returns_string<S: Into<String>>(x: S) -> String {
    CALLS_TAKES_S.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    x.into()
}

// --- case 2: type param `Res`, concrete return type `Result<u32, String>` ---
// After removing spaces, "Result<u32,String>" contains the substring "Res".
// The old check falsely rejected this. With `cache_err = true` the value type
// is the full `Result<u32, String>`, which is concrete and must be allowed.
static CALLS_TAKES_RES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[once(cache_err = true)]
fn takes_res_returns_result<Res: std::fmt::Display>(msg: Res) -> Result<u32, String> {
    CALLS_TAKES_RES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let s = msg.to_string();
    if s.is_empty() {
        Err("empty".to_string())
    } else {
        Ok(42)
    }
}

// --- case 3: type param `S` inside a bracket-group value type `[String; 2]` ---
// The value type is an array (`TokenTree::Group` with `[..]` delimiters), so the
// guard's whole-ident walk must descend into the group. `String` inside the group
// contains `S` as a substring but is not a whole-ident match, so this must NOT be
// rejected. Pins the substring-vs-whole-ident distinction on the group-descent
// path specifically (the negative fixture `[u8; N]` exercises the same descent for
// a genuine whole-ident match).
static CALLS_TAKES_S_ARR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[once]
fn takes_s_returns_arr<S: Into<String> + Clone>(x: S) -> [String; 2] {
    CALLS_TAKES_S_ARR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let s = x.into();
    [s.clone(), s]
}

// --- tests ---

#[test]
fn test_takes_s_returns_string_compiles_and_runs() {
    // First call executes the body; all subsequent calls return the cached value.
    let v1 = takes_s_returns_string("hello".to_string());
    assert_eq!(v1, "hello");
    // A second call with a different arg still returns the first-cached value.
    let v2 = takes_s_returns_string("world".to_string());
    assert_eq!(v2, "hello", "once cache must return first-cached value");
    assert_eq!(
        CALLS_TAKES_S.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "body must only execute once"
    );
}

#[test]
fn test_takes_res_returns_result_compiles_and_runs() {
    let v = takes_res_returns_result("ok");
    assert!(v.is_ok());
    assert_eq!(v.unwrap(), 42);
}

#[test]
fn test_takes_s_returns_arr_compiles_and_runs() {
    let v1 = takes_s_returns_arr("hi");
    assert_eq!(v1, ["hi".to_string(), "hi".to_string()]);
    // once cache returns the first-cached array on subsequent calls.
    let v2 = takes_s_returns_arr("bye");
    assert_eq!(
        v2,
        ["hi".to_string(), "hi".to_string()],
        "once cache must return first-cached value"
    );
    assert_eq!(
        CALLS_TAKES_S_ARR.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "body must only execute once"
    );
}
