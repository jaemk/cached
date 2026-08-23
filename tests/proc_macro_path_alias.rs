/*!
`cached::proc_macro` is an alias of `cached::macros`, kept because that is the module path
most proc-macro crates use and the one pre-1.0 `cached` released. Every macro must stay
reachable through all three spellings -- the crate root, `cached::macros`, and the
`cached::proc_macro` alias -- so a refactor of `mod macros` cannot silently break the alias
for downstream code that still imports through it.
*/

#![cfg(feature = "proc_macro")]

use cached::macros::concurrent_cached as concurrent_cached_via_macros;
use cached::macros::once as once_via_macros;
use cached::proc_macro::cached as cached_via_proc_macro;
use cached::proc_macro::concurrent_cached as concurrent_cached_via_proc_macro;
use cached::proc_macro::once as once_via_proc_macro;
use cached::{cached as cached_via_root, once as once_via_root};

#[cached_via_proc_macro]
fn double_alias(n: u64) -> u64 {
    n * 2
}

#[cached_via_root]
fn double_root(n: u64) -> u64 {
    n * 2
}

#[once_via_proc_macro]
fn config_alias() -> String {
    "cfg".to_string()
}

#[once_via_macros]
fn config_macros() -> String {
    "cfg".to_string()
}

#[once_via_root]
fn config_root() -> String {
    "cfg".to_string()
}

#[concurrent_cached_via_proc_macro]
fn triple_alias(n: u64) -> u64 {
    n * 3
}

#[concurrent_cached_via_macros]
fn triple_macros(n: u64) -> u64 {
    n * 3
}

/// `Return` is re-exported alongside the macros and must be nameable through the alias too.
const _: fn(cached::proc_macro::Return<u64>) -> bool = |r| r.was_cached();

#[test]
fn every_macro_resolves_through_the_proc_macro_alias() {
    assert_eq!(double_alias(21), 42);
    assert_eq!(double_alias(21), 42, "second call is served from the cache");
    assert_eq!(double_root(21), 42);

    assert_eq!(config_alias(), "cfg");
    assert_eq!(config_macros(), "cfg");
    assert_eq!(config_root(), "cfg");

    assert_eq!(triple_alias(7), 21);
    assert_eq!(triple_alias(7), 21, "second call is served from the cache");
    assert_eq!(triple_macros(7), 21);
}

/// The alias and the canonical path name the same module, not two copies of it.
#[test]
fn the_alias_and_the_canonical_module_are_the_same_item() {
    const _: fn(cached::macros::Return<u64>) -> cached::proc_macro::Return<u64> = |r| r;
}
