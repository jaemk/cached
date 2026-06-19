use darling::{Error, FromMeta};
use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::__private::Span;
use quote::{format_ident, quote};
use std::ops::Deref;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    Attribute, Block, FnArg, GenericArgument, Pat, PatType, PathArguments, ReturnType, Signature,
    Type, parse_quote, parse_str,
};

/// Resolve the path to the `cached` crate for use in generated code.
///
/// Generated code that referred to `::cached::...` broke for downstream crates
/// that renamed the dependency (e.g. `cached = { package = "cached", ... }` under
/// a different name) - issue #157. `proc-macro-crate` looks up the actual import
/// name from the dependent crate's `Cargo.toml`:
///
/// - `FoundCrate::Itself` (the macro is used inside `cached`'s own tests/examples)
///   resolves to `::cached`.
/// - `FoundCrate::Name(n)` resolves to `::n` (the renamed import).
/// - On error (no manifest / not found), fall back to `::cached` so the crate's
///   own test suite - where the lookup can fail - keeps working. This cannot be a
///   hard error: doctests and some build configs hit the `Err` path legitimately,
///   so propagating a diagnostic would break `cached`'s own build. The cost is
///   that a downstream crate that both renamed the dependency and trips the error
///   path gets an "unresolved import `::cached`" error rather than a manifest-lookup
///   message - a rare edge case where the import error is itself a usable signal.
pub(super) fn crate_path() -> TokenStream2 {
    match crate_name("cached") {
        Ok(FoundCrate::Itself) => quote! { ::cached },
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote! { ::#ident }
        }
        Err(_) => quote! { ::cached },
    }
}

/// Returns `true` if `output` is a `Result<...>` type (last path segment is
/// exactly `"Result"` and carries type arguments).
pub(super) fn is_result_return_type(output: &ReturnType) -> bool {
    match output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => match &**ty {
            Type::Path(tp) => tp.path.segments.last().is_some_and(|s| {
                !matches!(s.arguments, PathArguments::None) && s.ident == "Result"
            }),
            _ => false,
        },
    }
}

/// Returns `true` if `output` is an `Option<...>` type (last path segment is `"Option"` with type args).
pub(super) fn is_option_return_type(output: &ReturnType) -> bool {
    match output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => match &**ty {
            Type::Path(tp) => tp.path.segments.last().is_some_and(|s| {
                !matches!(s.arguments, PathArguments::None) && s.ident == "Option"
            }),
            _ => false,
        },
    }
}

/// The migration message emitted when `ttl` is given as a bare integer literal
/// (the old `ttl = 60` whole-seconds form). Shared by all three macros so the
/// message stays identical everywhere.
pub(super) const TTL_INT_MIGRATION_MESSAGE: &str = "`ttl` now takes a Duration expression (e.g. `ttl = \"Duration::from_secs(60)\"`); \
     for whole seconds use `ttl_secs = 60`, for milliseconds use `ttl_millis = 500`.";

/// Custom `FromMeta` type for the `ttl` macro attribute.
///
/// `ttl` now accepts a `Duration` expression written as a string literal (the
/// same convention as `create`/`convert`), e.g.
/// `ttl = "core::time::Duration::from_secs(60)"`. The string is stored verbatim
/// here and parsed into a `syn::Expr` later by the macro.
///
/// A bare integer literal (`ttl = 60`) was the old whole-seconds form. It is now
/// rejected with a helpful migration message pointing at `ttl_secs`/`ttl_millis`
/// (matching the crate's helpful-rename pattern), rather than darling's generic
/// "expected string" error.
#[derive(Debug, Clone)]
pub(super) struct TtlExpr {
    pub expr: String,
    pub span: Option<Span>,
}

impl FromMeta for TtlExpr {
    fn from_string(value: &str) -> darling::Result<Self> {
        Ok(Self {
            expr: value.to_string(),
            span: None,
        })
    }

    // Intercept any non-string literal (e.g. the old `ttl = 60` integer form)
    // and emit the migration message instead of darling's "expected string".
    fn from_value(value: &syn::Lit) -> darling::Result<Self> {
        match value {
            syn::Lit::Str(s) => Ok(Self {
                expr: s.value(),
                span: Some(s.span()),
            }),
            other => Err(darling::Error::custom(TTL_INT_MIGRATION_MESSAGE).with_span(other)),
        }
    }
}

/// Build the internal `ttl_duration` token and `has_ttl` flag from the three
/// mutually exclusive TTL attributes (`ttl` expr, `ttl_secs`, `ttl_millis`).
///
/// Returns `Ok((has_ttl, ttl_duration))` where `ttl_duration` is `Some` when any
/// TTL is set. Performs the 3-way mutual-exclusion check, the `ttl_secs >= 1` /
/// `ttl_millis >= 1` validation, and parses the `ttl` expression string.
pub(super) fn resolve_ttl_duration(
    krate: &TokenStream2,
    ttl: &Option<TtlExpr>,
    ttl_secs: Option<u64>,
    ttl_millis: Option<u64>,
    span: Span,
) -> Result<(bool, Option<TokenStream2>), syn::Error> {
    let set_count = usize::from(ttl.is_some())
        + usize::from(ttl_secs.is_some())
        + usize::from(ttl_millis.is_some());
    if set_count > 1 {
        return Err(syn::Error::new(
            span,
            "`ttl`, `ttl_secs`, and `ttl_millis` are mutually exclusive - \
             `ttl` takes a `Duration` expression, `ttl_secs` whole seconds, \
             `ttl_millis` milliseconds; use exactly one",
        ));
    }
    if matches!(ttl_secs, Some(0)) {
        return Err(syn::Error::new(span, "`ttl_secs` must be >= 1"));
    }
    if matches!(ttl_millis, Some(0)) {
        return Err(syn::Error::new(span, "`ttl_millis` must be >= 1"));
    }
    let ttl_duration = if let Some(ttl_expr) = ttl {
        let err_span = ttl_expr.span.unwrap_or(span);
        let expr = parse_str::<syn::Expr>(&ttl_expr.expr).map_err(|error| {
            syn::Error::new(
                err_span,
                format!(
                    "unable to parse `ttl` as a Duration expression: {error}; \
                     `ttl` takes a `Duration` expression as a string literal, e.g. \
                     `ttl = \"core::time::Duration::from_secs(60)\"`"
                ),
            )
        })?;
        Some(quote! { #expr })
    } else if let Some(secs) = ttl_secs {
        Some(quote! { #krate::time::Duration::from_secs(#secs) })
    } else {
        ttl_millis.map(|millis| quote! { #krate::time::Duration::from_millis(#millis) })
    };
    Ok((ttl_duration.is_some(), ttl_duration))
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub(super) enum SyncWriteMode {
    #[default]
    Disabled,
    Default,
    ByKey,
}

impl FromMeta for SyncWriteMode {
    fn from_word() -> darling::Result<Self> {
        Ok(Self::Default)
    }

    fn from_bool(value: bool) -> darling::Result<Self> {
        Ok(if value { Self::Default } else { Self::Disabled })
    }

    fn from_string(value: &str) -> darling::Result<Self> {
        match value {
            "default" | "true" => Ok(Self::Default),
            "by_key" => Ok(Self::ByKey),
            "false" => Ok(Self::Disabled),
            _ => Err(Error::unknown_value(value)),
        }
    }
}

pub(super) fn validate_sync_writes_buckets(
    buckets: usize,
    span: proc_macro2::Span,
) -> std::result::Result<(), syn::Error> {
    if buckets == 0 {
        Err(syn::Error::new(
            span,
            "`sync_writes_buckets` must be greater than 0",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn by_key_lock_block(
    key: TokenStream2,
    locks: TokenStream2,
    lock_method: TokenStream2,
    await_if_async: TokenStream2,
) -> TokenStream2 {
    quote! {
        let lock = {
            use std::hash::{Hash, Hasher};
            // DefaultHasher is used for bucket selection only. It is not cryptographic and
            // has no cross-version stability guarantees, but only within-process consistency
            // is required for runtime lock-bucket selection.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            #key.hash(&mut hasher);
            #locks[(hasher.finish() as usize) % #locks.len()].clone()
        };
        let _key_lock = lock.#lock_method()#await_if_async;
    }
}

// if you define arguments as mutable, e.g.
// #[cached]
// fn mutable_args(mut a: i32, mut b: i32) -> (i32, i32) {
//     a += 1;
//     b += 1;
//     (a, b)
// }
// then we want the `mut` keywords present on the "inner" function
// that wraps your actual block of code.
// If the `mut`s are also on the outer method, then you'll
// get compiler warnings about your arguments not needing to be `mut`
// when they really do need to be.
pub(super) fn get_mut_signature(signature: Signature) -> Signature {
    let mut signature_no_muts = signature;
    let mut sig_inputs = Punctuated::new();
    for inp in &signature_no_muts.inputs {
        let item = match inp {
            FnArg::Receiver(_) => inp.clone(),
            FnArg::Typed(pat_type) => {
                let mut pt = pat_type.clone();
                let pat = match_pattern_type(&pat_type);
                pt.pat = pat;
                FnArg::Typed(pt)
            }
        };
        sig_inputs.push(item);
    }
    signature_no_muts.inputs = sig_inputs;
    signature_no_muts
}

pub(super) fn match_pattern_type(pat_type: &&PatType) -> Box<Pat> {
    match &pat_type.pat.deref() {
        Pat::Ident(pat_ident) => {
            if pat_ident.mutability.is_some() {
                let mut p = pat_ident.clone();
                p.mutability = None;
                Box::new(Pat::Ident(p))
            } else {
                Box::new(Pat::Ident(pat_ident.clone()))
            }
        }
        _ => pat_type.pat.clone(),
    }
}

// Find the type of the value to store.
// Normally it's the same as the return type of the functions, but
// for Options and Results it's the (first) inner type. So for
// Option<u32>, store u32, for Result<i32, String>, store i32, etc.
pub(super) fn find_value_type(
    is_smart_result: bool,
    is_smart_option: bool,
    output: &ReturnType,
    output_ty: TokenStream2,
) -> Result<TokenStream2, syn::Error> {
    use syn::spanned::Spanned;
    match (is_smart_result, is_smart_option) {
        (false, false) => Ok(output_ty),
        (true, true) => Err(syn::Error::new(
            output_ty.span(),
            "a return type cannot be detected as both `Result<T, E>` and `Option<T>`",
        )),
        _ => match output.clone() {
            ReturnType::Default => Err(syn::Error::new(
                output_ty.span(),
                "function must return a `Result<T, E>` or `Option<T>` for its inner value to be cached",
            )),
            ReturnType::Type(_, ty) => {
                let span = ty.span();
                if let Type::Path(typepath) = *ty {
                    let segments = typepath.path.segments;
                    if let Some(last_seg) = segments.last() {
                        if let PathArguments::AngleBracketed(brackets) = &last_seg.arguments {
                            if let Some(inner_ty) = brackets.args.first() {
                                Ok(quote! {#inner_ty})
                            } else {
                                Err(syn::Error::new(
                                    span,
                                    "function return type has no inner type",
                                ))
                            }
                        } else {
                            Err(syn::Error::new(
                                span,
                                "function return type has no inner type",
                            ))
                        }
                    } else {
                        Err(syn::Error::new(span, "function return type is too complex"))
                    }
                } else {
                    Err(syn::Error::new(span, "function return type is too complex"))
                }
            }
        },
    }
}

/// Extracts the single angle-bracketed type argument from a path type's last
/// segment - e.g. the `T` in `Result<T, E>` or `Return<T>`. `not_path` is the
/// error message when `ty` is not a simple path type; `no_arg` is the message
/// when the path has no usable `<...>` argument. Used by `#[concurrent_cached]`
/// to peel `Result` (and, with `with_cached_flag`, `cached::Return`).
pub(super) fn first_type_arg<'a>(
    ty: &'a Type,
    span: Span,
    not_path: &str,
    no_arg: &str,
) -> Result<&'a GenericArgument, syn::Error> {
    let Type::Path(typepath) = ty else {
        return Err(syn::Error::new(span, not_path));
    };
    let Some(segment) = typepath.path.segments.last() else {
        return Err(syn::Error::new(span, no_arg));
    };
    let PathArguments::AngleBracketed(brackets) = &segment.arguments else {
        return Err(syn::Error::new(span, no_arg));
    };
    brackets
        .args
        .first()
        .ok_or_else(|| syn::Error::new(span, no_arg))
}

// make the cache key type and block that converts the inputs into the key type
pub(super) fn make_cache_key_type(
    key: &Option<String>,
    convert: &Option<syn::Expr>,
    ty: &Option<String>,
    input_tys: Vec<Type>,
    input_names: &[Pat],
) -> Result<(TokenStream2, TokenStream2), syn::Error> {
    match (key, convert, ty) {
        (Some(key_str), Some(convert_expr), _) => {
            let cache_key_ty = parse_str::<Type>(key_str).map_err(|error| {
                syn::Error::new(
                    Span::call_site(),
                    format!(
                        "unable to parse `key` as a type: {error}; \
                         `key` must be a Rust type, e.g. `key = \"String\"` or \
                         `key = \"(u32, String)\"`"
                    ),
                )
            })?;

            let key_convert_block = expr_to_block(convert_expr.clone());

            Ok((quote! {#cache_key_ty}, quote! {#key_convert_block}))
        }
        (None, Some(convert_expr), Some(_)) => {
            let key_convert_block = expr_to_block(convert_expr.clone());

            Ok((quote! {}, quote! {#key_convert_block}))
        }
        (None, None, _) => {
            // Default key: derive an owned key type + conversion from the
            // function inputs. Reference inputs (`&T`/`&mut T` and
            // `Option<&T>`/`Option<&mut T>`) are converted to owned key components
            // so the cache can store them without borrowing from the call
            // (#202/#203). The owned type is `<T as ToOwned>::Owned` (so `&str`
            // keys on `String`, `&[u8]` on `Vec<u8>`, `&Foo: Clone` on `Foo`):
            //   `&T` / `&mut T`                 -> key type `<T as ToOwned>::Owned`,         expr `name.to_owned()`
            //   `Option<&T>` / `Option<&mut T>` -> key type `Option<<T as ToOwned>::Owned>`, expr `name.as_deref().map(|__cached_v| __cached_v.to_owned())`
            //   otherwise                       -> key type `T`,                             expr `name.clone()`
            let mut key_tys: Vec<TokenStream2> = Vec::with_capacity(input_tys.len());
            let mut key_exprs: Vec<TokenStream2> = Vec::with_capacity(input_tys.len());
            for (ty, name) in input_tys.iter().zip(input_names.iter()) {
                if let Some(inner) = strip_ref(ty) {
                    key_tys.push(quote! { <#inner as ::std::borrow::ToOwned>::Owned });
                    key_exprs.push(quote! { #name.to_owned() });
                } else if let Some(inner) = option_ref_inner(ty) {
                    key_tys.push(quote! { Option<<#inner as ::std::borrow::ToOwned>::Owned> });
                    // Use `as_deref()` to avoid moving `name` (Option<&mut T> is not
                    // Copy, and `.map()` would move it, causing a use-after-move error
                    // when `name` is reused in the `_no_cache` call). `as_deref` takes
                    // `&self` and yields `Option<&T>` for both `Option<&T>` and
                    // `Option<&mut T>` without consuming the Option (#FIX-C).
                    key_exprs
                        .push(quote! { #name.as_deref().map(|__cached_v| __cached_v.to_owned()) });
                } else {
                    key_tys.push(quote! { #ty });
                    key_exprs.push(quote! { #name.clone() });
                }
            }
            // Match the original parenthesized-list shape (no trailing comma):
            // a single input yields the bare element type `(T)` == `T` and expr
            // `(name...)`, exactly as before; multiple inputs yield a tuple.
            Ok((quote! {(#(#key_tys),*)}, quote! {(#(#key_exprs),*)}))
        }
        (Some(_), None, _) => Err(syn::Error::new(
            Span::call_site(),
            "`key` requires `convert` to be set",
        )),
        (None, Some(_), None) => Err(syn::Error::new(
            Span::call_site(),
            "`convert` requires `key` or `ty` to be set",
        )),
    }
}

/// If `ty` is a reference `&T` (or `&mut T`), return the referent `T`.
/// Used by the default-key path to derive an owned key component (#202).
fn strip_ref(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Reference(r) => Some(&r.elem),
        _ => None,
    }
}

/// If `ty` is `Option<&T>` (or `Option<&mut T>`, including qualified
/// `std::option::Option`), return the referent `T`. Used by the default-key
/// path so `Option<&str>` keys on an owned `Option<String>` (#203).
fn option_ref_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    strip_ref(inner)
}

// if you define arguments as mutable, e.g.
// #[once]
// fn mutable_args(mut a: i32, mut b: i32) -> (i32, i32) {
//     a += 1;
//     b += 1;
//     (a, b)
// }
// then we need to strip off the `mut` keyword from the
// variable identifiers, so we can refer to arguments `a` and `b`
// instead of `mut a` and `mut b`
pub(super) fn get_input_names(inputs: &Punctuated<FnArg, Comma>) -> Vec<Pat> {
    inputs
        .iter()
        // Skip the receiver (`self`/`&self`/`&mut self`): it is not a keyable
        // argument. `in_impl = true` / a custom `convert` allow `self` methods,
        // and the `self.` prefix is re-prepended at the call site (#16/#140).
        .filter_map(|input| match input {
            FnArg::Receiver(_) => None,
            FnArg::Typed(pat_type) => Some(*match_pattern_type(&pat_type)),
        })
        .collect()
}

pub(super) fn fill_in_attributes(attributes: &mut Vec<Attribute>, cache_fn_doc_extra: String) {
    if attributes.iter().any(|attr| attr.path().is_ident("doc")) {
        attributes.push(parse_quote! { #[doc = ""] });
        attributes.push(parse_quote! { #[doc = "# Caching"] });
        attributes.push(parse_quote! { #[doc = #cache_fn_doc_extra] });
    } else {
        attributes.push(parse_quote! { #[doc = #cache_fn_doc_extra] });
    }
}

// pull out the names and types of the function inputs
pub(super) fn get_input_types(inputs: &Punctuated<FnArg, Comma>) -> Vec<Type> {
    inputs
        .iter()
        // Skip the receiver (see `get_input_names`): `self` is not a keyable arg.
        .filter_map(|input| match input {
            FnArg::Receiver(_) => None,
            FnArg::Typed(pat_type) => Some(*pat_type.ty.clone()),
        })
        .collect()
}

pub(super) fn with_cache_flag_error(output_span: Span, output_type_display: String) -> TokenStream {
    syn::Error::new(
        output_span,
        format!(
            "\nWhen specifying `with_cached_flag = true`, \
                    the return type must be wrapped in `cached::Return<T>`. \n\
                    The following return types are supported: \n\
                    |    `cached::Return<T>`\n\
                    |    `std::result::Result<cached::Return<T>, E>`\n\
                    |    `std::option::Option<cached::Return<T>>`\n\
                    Found type: {t}.",
            t = output_type_display
        ),
    )
    .to_compile_error()
    .into()
}

/// Parse the `force_refresh` expression (`Option<syn::Expr>`, already parsed by
/// darling) into an `Option<syn::Block>` for use in generated code.
///
/// Returns `Ok(None)` when `force_refresh` is `None`. Shared by
/// `build_force_refresh_guard` and by `#[concurrent_cached]`, which needs the
/// same parsed block to build its `force_refresh_bypass` token, so the expression
/// is extracted only once per macro expansion.
///
/// If the `Expr` is already `Expr::Block`, its inner `Block` is used directly.
/// Otherwise the expression is wrapped in a synthetic block so a bare expression
/// (e.g. `force_refresh = { id == 0 }`) also works.
pub(super) fn parse_force_refresh_block(
    force_refresh: &Option<syn::Expr>,
    _span: Span,
) -> Result<Option<Block>, syn::Error> {
    match force_refresh {
        Some(expr) => {
            let block = expr_to_block(expr.clone());
            Ok(Some(block))
        }
        None => Ok(None),
    }
}

/// Convert a `syn::Expr` to a `syn::Block`.
///
/// If `expr` is already `Expr::Block`, return its inner block directly.
/// Otherwise wrap it in a synthetic block `{ expr }`.
pub(super) fn expr_to_block(expr: syn::Expr) -> Block {
    use syn::{Stmt, parse_quote};
    match expr {
        syn::Expr::Block(eb) => eb.block,
        other => {
            let stmt: Stmt = parse_quote! { #other };
            Block {
                brace_token: Default::default(),
                stmts: vec![stmt],
            }
        }
    }
}

/// Emit an attribute expression for *value/argument* position (e.g. `create`,
/// `cache_prefix_block`, which expand into `Lock::new(<here>)` / `.prefix(<here>)`).
///
/// A single-expression block (`{ Store::builder()...build().unwrap() }`, the natural
/// unquoted spelling, or the parsed legacy quoted form) is unwrapped to its inner
/// expression so the generated code is `Lock::new(Store::builder()...)` rather than
/// `Lock::new({ Store::builder()... })` (which trips `unused_braces` under `-D warnings`).
/// Bare expressions are emitted directly; multi-statement blocks are kept as-is (the
/// braces are load-bearing and `unused_braces` does not flag them).
pub(super) fn expr_value_tokens(expr: &syn::Expr) -> TokenStream2 {
    if let syn::Expr::Block(eb) = expr
        && eb.attrs.is_empty()
        && eb.label.is_none()
        && eb.block.stmts.len() == 1
        && let syn::Stmt::Expr(inner, None) = &eb.block.stmts[0]
    {
        return quote! { #inner };
    }
    quote! { #expr }
}

/// Build the `force_refresh` guard token that wraps a cached-hit early return.
///
/// `force_refresh` is an opt-in boolean expression block over the function args,
/// in curly braces like `convert` (e.g. `force_refresh = { id == 0 }` or the
/// legacy quoted form `force_refresh = "{ id == 0 }"`). When it evaluates to
/// `true`, the cached-hit early return is skipped so the body re-runs and
/// re-caches. The returned token is `if !(block)`; with no `force_refresh` it is
/// `if true` (always take the cached value). Orthogonal to `refresh` (TTL renewal
/// on hit) (#146). Shared by `#[cached]`, `#[concurrent_cached]`, and `#[once]`.
pub(super) fn build_force_refresh_guard(
    force_refresh: &Option<syn::Expr>,
    span: Span,
) -> Result<TokenStream2, syn::Error> {
    match parse_force_refresh_block(force_refresh, span)? {
        Some(block) => Ok(quote! { if !(#block) }),
        None => Ok(quote! { if true }),
    }
}

pub(super) fn gen_return_cache_block(
    krate: &TokenStream2,
    ttl_duration: Option<TokenStream2>,
    expires: bool,
    return_cache_block: TokenStream2,
) -> TokenStream2 {
    if expires {
        quote! {
            if !<_ as #krate::Expires>::is_expired(__cached_result) {
                #return_cache_block
            }
        }
    } else if let Some(ttl_duration) = &ttl_duration {
        quote! {
            let (__cached_created_sec, __cached_result) = __cached_result;
            if __cached_now.saturating_duration_since(*__cached_created_sec) < #ttl_duration {
                #return_cache_block
            }
        }
    } else {
        quote! { #return_cache_block }
    }
}

// Structurally check that `ty` is `cached::Return<T>` (or unqualified
// `Return<T>`), descending through a single outer `Result<_, _>` / `Option<_>`
// wrapper via its first type argument. A proc macro sees tokens, not resolved
// types, so this still cannot see through a type alias
// (e.g. `use cached::Return as R;`) - but it correctly rejects an unrelated
// `Return` from another module (e.g. `other::Return<T>`) instead of accepting
// it and failing later with a confusing error.
fn type_is_cached_return(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    let Some(last) = type_path.path.segments.last() else {
        return false;
    };
    match last.ident.to_string().as_str() {
        "Result" | "Option" => {
            if let PathArguments::AngleBracketed(bracketed) = &last.arguments {
                bracketed
                    .args
                    .iter()
                    .find_map(|arg| match arg {
                        GenericArgument::Type(inner) => Some(inner),
                        _ => None,
                    })
                    .is_some_and(type_is_cached_return)
            } else {
                false
            }
        }
        "Return" => {
            let segments: Vec<String> = type_path
                .path
                .segments
                .iter()
                .map(|seg| seg.ident.to_string())
                .collect();
            matches!(segments.as_slice(), [r] if r == "Return")
                || matches!(segments.as_slice(), [c, r] if c == "cached" && r == "Return")
        }
        _ => false,
    }
}

// if `with_cached_flag = true`, then enforce that the return type
// is something wrapped in `Return`. Either `Return<T>` or the
// fully qualified `cached::Return<T>`, optionally inside a single
// `Result<_, _>` / `Option<_>` wrapper.
pub(super) fn check_with_cache_flag(with_cached_flag: bool, output: &ReturnType) -> bool {
    if !with_cached_flag {
        return false;
    }
    match output {
        // `()` / no return type can never be `Return<T>`
        ReturnType::Default => true,
        ReturnType::Type(_, ty) => !type_is_cached_return(ty),
    }
}
