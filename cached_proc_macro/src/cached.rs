use crate::helpers::*;
use darling::FromMeta;
use darling::ast::NestedMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Ident, ItemFn, ReturnType, Type, parse_macro_input, parse_str};

#[derive(Debug, Default, Eq, PartialEq)]
enum SyncLock {
    #[default]
    Mutex,
    RwLock,
}

// Hand-written so both the documented `"rwlock"` spelling and darling's
// derived snake_case `"rw_lock"` are accepted (the derive only accepted the
// latter, so the publicly-documented `sync_lock = "rwlock"` failed to compile).
impl FromMeta for SyncLock {
    fn from_string(value: &str) -> darling::Result<Self> {
        match value {
            "mutex" => Ok(Self::Mutex),
            "rwlock" | "rw_lock" => Ok(Self::RwLock),
            _ => Err(darling::Error::unknown_value(value)),
        }
    }
}

#[derive(FromMeta)]
struct CachedMacroArgs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    unbound: bool,
    /// Sets the maximum number of cached entries (the LRU bound).
    /// Mirrors the `max_size` builder/constructor naming on the cache stores.
    #[darling(default)]
    max_size: Option<usize>,
    /// A cache TTL expressed as a `Duration` expression in a string literal
    /// (same convention as `create`/`convert`), e.g.
    /// `ttl = "core::time::Duration::from_secs(60)"`. Mutually exclusive with
    /// `ttl_secs`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl: Option<TtlExpr>,
    /// TTL in whole seconds. Convenience alternative to `ttl`. Mutually
    /// exclusive with `ttl`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl_secs: Option<u64>,
    /// TTL in milliseconds. A finer-grained alternative to `ttl_secs`. Mutually
    /// exclusive with `ttl`, `ttl_secs`, and `expires` (#149).
    #[darling(default)]
    ttl_millis: Option<u64>,
    #[darling(default)]
    refresh: bool,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    time_refresh: Option<bool>,
    /// Removed alias for `max_size`; kept only to emit a helpful rename error.
    #[darling(default)]
    size: Option<usize>,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<syn::Expr>,
    #[darling(default)]
    cache_err: bool,
    #[darling(default)]
    cache_none: bool,
    /// `None` = not specified by user (defaults to `ByKey` for `#[cached]`).
    /// `Some(Disabled)` = explicit `sync_writes = false`.
    /// `Some(Default)` = explicit `sync_writes = true` / `"default"`.
    /// `Some(ByKey)` = explicit `sync_writes = "by_key"`.
    #[darling(default)]
    sync_writes: Option<SyncWriteMode>,
    #[darling(default = "default_sync_writes_buckets")]
    sync_writes_buckets: usize,
    #[darling(default)]
    sync_lock: Option<SyncLock>,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<syn::Expr>,
    #[darling(default)]
    result_fallback: bool,
    #[darling(default)]
    unsync_reads: bool,
    #[darling(default)]
    expires: bool,
    /// A boolean expression over the function arguments; when it evaluates to
    /// `true`, the cached value (if any) is bypassed and the function body is
    /// re-run and re-cached. Orthogonal to `refresh` (which renews a TTL on a
    /// cache hit). Both unquoted `{ expr }` and legacy quoted `"{ expr }"` forms
    /// are accepted (#146).
    #[darling(default)]
    force_refresh: Option<syn::Expr>,
    /// Override the visibility of the companion fns (`{fn}_no_cache`,
    /// `{fn}_prime_cache`). Parsed as a `syn::Visibility` string. `None` (default)
    /// inherits the cached fn's visibility. `""` means private.
    #[darling(default)]
    companions_vis: Option<String>,
    /// Allow the macro on a method that takes `self` inside an `impl` block.
    /// The cache static is emitted inside the generated fn body (legal there)
    /// and the receiver is preserved/forwarded (#16/#140).
    #[darling(default)]
    in_impl: bool,
    // Removed attributes intercepted to provide helpful error messages
    #[darling(default)]
    result: Option<bool>,
    #[darling(default)]
    option: Option<bool>,
}

fn default_sync_writes_buckets() -> usize {
    64
}

/// When a `create` block is supplied the user fully constructs the store, so the
/// store-builder attributes the macro would otherwise apply are dropped. Reject
/// those attributes with a precise message instead of silently ignoring them -
/// otherwise (e.g. with `ttl_millis`) the store-type match no longer reaches the
/// `create` arm and the user sees the generic "cache types are mutually
/// exclusive" message rather than a specific one. Mirrors
/// `#[concurrent_cached]`'s `check_create_conflicts`.
fn check_create_conflicts(
    args: &CachedMacroArgs,
    span: proc_macro2::Span,
) -> Result<(), syn::Error> {
    let mut conflicting = Vec::new();
    if args.ttl.is_some() {
        conflicting.push("ttl");
    }
    if args.ttl_secs.is_some() {
        conflicting.push("ttl_secs");
    }
    if args.ttl_millis.is_some() {
        conflicting.push("ttl_millis");
    }
    if args.refresh {
        conflicting.push("refresh");
    }
    if args.max_size.is_some() {
        conflicting.push("max_size");
    }
    if conflicting.is_empty() {
        return Ok(());
    }
    let list = conflicting
        .iter()
        .map(|a| format!("`{a}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(syn::Error::new(
        span,
        format!(
            "cannot specify {list} when passing a `create` block - `create` fully \
             constructs the store, so these store-builder attributes would be \
             silently ignored"
        ),
    ))
}

pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    if let Err(e) = reject_concurrent_only_attrs("cached", &attr_args) {
        return e.to_compile_error().into();
    }
    let args = match CachedMacroArgs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };
    let input = parse_macro_input!(input as ItemFn);

    // pull out the parts of the input
    let mut attributes = input.attrs;
    let visibility = input.vis;
    let signature = input.sig;
    let body = input.block;

    // Resolve the path to the `cached` crate so generated code works when the
    // dependency is renamed by the downstream crate (#157).
    let krate = crate_path();

    // Resolve the effective sync_writes mode.
    // `None` (unspecified by user) defaults to `ByKey` for `#[cached]`.
    // Explicit `sync_writes = false` => Disabled; `= true`/`"default"` => Default;
    // `= "by_key"` => ByKey.
    let sync_writes_explicit = args.sync_writes.is_some();
    let sync_writes = args.sync_writes.unwrap_or(SyncWriteMode::ByKey);

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness;
    let has_receiver = inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Receiver(_)));

    // Resolve companion fn visibility (#9). `companions_vis = None` inherits the
    // cached fn's visibility. `companions_vis = Some(s)` parses `s` as a
    // `syn::Visibility`; an empty string produces private visibility.
    let companions_visibility = match &args.companions_vis {
        None => quote! { #visibility },
        Some(s) if s.is_empty() => quote! {},
        Some(s) => match parse_str::<syn::Visibility>(s) {
            Ok(vis) => quote! { #vis },
            Err(e) => {
                return syn::Error::new(
                    fn_ident.span(),
                    format!(
                        "unable to parse `companions_vis` as a visibility: {e}; \
                         expected a Rust visibility, e.g. `\"pub\"`, `\"pub(crate)\"`, or `\"\"`"
                    ),
                )
                .to_compile_error()
                .into();
            }
        },
    };

    // Reject `self` methods unless `in_impl = true`. A `self` receiver only
    // exists inside an `impl`/trait, and off the `in_impl` path the cache static
    // is emitted at that same scope, where a `static` is not a valid item - so a
    // `convert` block alone cannot rescue a `self` method (it would still fail
    // later with an opaque error). `in_impl` is the only fix (#16/#140).
    if has_receiver && !args.in_impl {
        return syn::Error::new(
            fn_ident.span(),
            "#[cached] cannot be applied to methods that take `self`. \
             Set `in_impl = true` to cache the method inside its `impl` block \
             (a `convert` block alone is not sufficient: the generated cache \
             static cannot live at `impl` scope).",
        )
        .to_compile_error()
        .into();
    }

    // The inverse: `in_impl = true` on a function with no `self` receiver
    // mis-compiles, because the generated `{fn}_no_cache(args)` call inside the
    // impl cannot resolve without a `Self::` qualifier (a confusing "cannot find
    // function" error downstream). Reject it here with a clear message.
    if args.in_impl && !has_receiver {
        return syn::Error::new(
            fn_ident.span(),
            "in_impl = true requires a method with a `self` receiver; \
             for a free function or an associated function without `self`, \
             remove in_impl.",
        )
        .to_compile_error()
        .into();
    }

    // Generic functions need the cache key pinned to a concrete type: the cache
    // is a single monomorphic `static` and cannot name the function's type
    // parameters. With an explicit `key` + `convert` the key type is concrete
    // and generics work (see the generic-where tests). Without `convert` the
    // default-key path would embed the type parameters in the key type, which
    // cannot compile - reject it with a clear diagnostic and workaround (#80).
    if (signature.generics.type_params().next().is_some()
        || signature.generics.const_params().next().is_some())
        && args.convert.is_none()
    {
        return syn::Error::new(
            fn_ident.span(),
            "#[cached] on a generic function requires `key` + `convert` to pin the cache key to a \
             concrete type: the cache is a single monomorphic static shared across all \
             instantiations and cannot name the function's type parameters. \
             Provide `key`/`convert` (and `ty`/`create` if the value type is also generic), or \
             wrap the generic function in a non-generic `#[cached]` function per concrete type.",
        )
        .to_compile_error()
        .into();
    }

    // Reject zero `max_size`/`ttl` at expansion time (matching `#[concurrent_cached]`),
    // rather than letting the generated builder `build()` panic at first call.
    if matches!(args.max_size, Some(0)) {
        return syn::Error::new(fn_ident.span(), "`max_size` must be >= 1")
            .to_compile_error()
            .into();
    }
    // Run the `expires`-vs-ttl mutual-exclusion checks BEFORE resolving the TTL
    // `Duration`. These need only presence (`is_some()`), not a parsed value, and
    // surfacing "mutually exclusive" is more relevant than a `ttl` parse error
    // when `expires` is also set.
    if args.expires && args.ttl_secs.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ttl_secs` are mutually exclusive - \
             `expires` delegates expiry to the value via the `Expires` trait; \
             `ttl_secs` applies a uniform time-based TTL to all entries",
        )
        .to_compile_error()
        .into();
    }
    if args.expires && args.ttl_millis.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ttl_millis` are mutually exclusive - \
             `expires` delegates expiry to the value via the `Expires` trait; \
             `ttl_millis` applies a uniform millisecond TTL to all entries",
        )
        .to_compile_error()
        .into();
    }
    if args.expires && args.ttl.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ttl` are mutually exclusive - \
             `expires` delegates expiry to the value via the `Expires` trait; \
             `ttl` applies a uniform time-based TTL to all entries",
        )
        .to_compile_error()
        .into();
    }
    // Resolve the TTL `Duration` token from whichever of `ttl` (expr), `ttl_secs`,
    // or `ttl_millis` is set. This performs the 3-way mutual-exclusion check, the
    // `ttl_secs`/`ttl_millis` >= 1 validation, and parses the `ttl` expression.
    let (has_ttl, ttl_duration) = match resolve_ttl_duration(
        &krate,
        &args.ttl,
        args.ttl_secs,
        args.ttl_millis,
        fn_ident.span(),
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    if args.time.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time` was renamed in a prior major release; use `ttl_secs = ...` \
             (or `ttl = \"Duration::from_secs(...)\"` / `ttl_millis = ...`)",
        )
        .to_compile_error()
        .into();
    }

    if args.time_refresh.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time_refresh` was renamed in a prior major release; use `refresh = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.unbound {
        return syn::Error::new(
            fn_ident.span(),
            "the `unbound` attribute has been removed. The default store (no `max_size`, \
             `ttl`, or `expires`) is already an `UnboundCache`, so use `#[cached]` without `unbound`.",
        )
        .to_compile_error()
        .into();
    }

    if args.size.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`size` was renamed to `max_size`; use `max_size = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.result.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "the `result` attribute has been removed. `Result<T, E>` returns now skip caching \
             `Err` by default. Remove `result = true` (or `result = false`), or use \
             `cache_err = true` to force-cache `Err` values.",
        )
        .to_compile_error()
        .into();
    }

    if args.option.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "the `option` attribute has been removed. `Option<T>` returns now skip caching \
             `None` by default. Remove `option = true` (or `option = false`), or use \
             `cache_none = true` to force-cache `None` values.",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.ty.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ty` are mutually exclusive - \
             `expires` generates the store type automatically",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.create.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `create` are mutually exclusive - \
             `expires` generates the store constructor automatically",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.with_cached_flag {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `with_cached_flag` are mutually exclusive - \
             the `Return<T>` wrapper does not implement `Expires`",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.unsync_reads {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `unsync_reads` are mutually exclusive - \
             `ExpiringCache` and `ExpiringLruCache` do not implement `CachedRead`",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.refresh {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `refresh` are mutually exclusive - \
             `refresh` renews a TTL on cache hit, but `ExpiringCache` and \
             `ExpiringLruCache` have no TTL to refresh; expiry is controlled by the value",
        )
        .to_compile_error()
        .into();
    }

    // `refresh = true` renews a TTL on cache hit. The default `UnboundCache`/`LruCache`
    // stores have no TTL to renew, so reject `refresh` unless a TTL is set (mirrors the
    // check in `concurrent_cached.rs`). `expires` is handled by the dedicated
    // mutual-exclusion check above, so exclude it here to avoid a confusing double error.
    // When a `create` block is supplied, the store is user-constructed and `refresh` is
    // rejected by `check_create_conflicts` below with a more specific message; skip here.
    if args.refresh && !has_ttl && !args.expires && args.create.is_none() {
        return syn::Error::new(
            fn_ident.span(),
            "`refresh` requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) to be set - \
             `refresh` renews a TTL on cache hit, but the default `UnboundCache`/`LruCache` \
             stores have no TTL to renew",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.cache_none {
        return syn::Error::new(
            fn_ident.span(),
            "`expires = true` and `cache_none = true` are incompatible - `expires` requires \
             the cache value type to implement `Expires`, but `cache_none = true` stores \
             `Option<V>` as the value, which does not implement `Expires`. \
             Remove `cache_none = true` (None values are not cached by default with `expires = true`).",
        )
        .to_compile_error()
        .into();
    }
    if args.expires && args.cache_err {
        return syn::Error::new(
            fn_ident.span(),
            "`expires = true` and `cache_err = true` are incompatible - `expires` requires \
             the cache value type to implement `Expires`, but `cache_err = true` stores \
             `Result<V, E>` as the value, which does not implement `Expires`. \
             Remove `cache_err = true` (Err values are not cached by default with `expires = true`).",
        )
        .to_compile_error()
        .into();
    }

    let input_tys = get_input_types(&inputs);
    let input_names = get_input_names(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let output_span = output_ty.span();
    let output_ts = TokenStream::from(output_ty.clone());
    let output_type_display = output_ts.to_string().replace(' ', "");

    if check_with_cache_flag(args.with_cached_flag, &output) {
        return with_cache_flag_error(output_span, output_type_display);
    }

    let is_result_return = is_result_return_type(&output);
    let is_option_return = is_option_return_type(&output);

    if args.cache_err && !is_result_return {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_err = true` requires the function to return `Result<T, E>`",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_none && !is_option_return {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_none = true` requires the function to return `Option<T>`",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_err && args.result_fallback {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_err` and `result_fallback` are mutually exclusive",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_none && args.with_cached_flag {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_none = true` and `with_cached_flag = true` are structurally incompatible \
             on `Option<T>` returns: `with_cached_flag` stores the inner `T` from `Return<T>` \
             while `cache_none = true` stores `Option<T>` as the cached value - the same \
             cache entry cannot hold both types. Use `with_cached_flag = true` alone (to get \
             cache-state flags; `None` is not cached by default), or use `cache_none = true` \
             alone (to force-cache `None` values).",
        )
        .to_compile_error()
        .into();
    }

    // `is_smart_result`: cache only Ok, skip Err (default for Result returns; opt out with cache_err)
    // `is_smart_option`: cache only Some, skip None (default for Option returns; opt out with cache_none)
    let is_smart_result = is_result_return && !args.cache_err;
    let is_smart_option = is_option_return && !args.cache_none;

    let cache_value_ty = match find_value_type(is_smart_result, is_smart_option, &output, output_ty)
    {
        Ok(value_ty) => value_ty,
        Err(e) => return e.to_compile_error().into(),
    };

    // make the cache identifier
    let cache_ident = match args.name {
        Some(ref name) => {
            if syn::parse_str::<syn::Ident>(name).is_err() {
                return syn::Error::new(fn_ident.span(), "`name` must be a valid Rust identifier")
                    .to_compile_error()
                    .into();
            }
            // G2: `__cached` prefix is reserved for macro-generated bindings.
            if name.starts_with("__cached") {
                return syn::Error::new(
                    fn_ident.span(),
                    "cache names beginning with `__cached` are reserved for macro-generated \
                     bindings and cannot be used as a `name` value",
                )
                .to_compile_error()
                .into();
            }
            Ident::new(name, fn_ident.span())
        }
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    let (cache_key_ty, key_convert_block) =
        match make_cache_key_type(&args.key, &args.convert, &args.ty, input_tys, &input_names) {
            Ok(key) => key,
            Err(error) => return error.to_compile_error().into(),
        };

    // `has_ttl` / `ttl_duration` were resolved above (from `ttl` expr, `ttl_secs`,
    // or `ttl_millis`). `has_ttl` drives store selection and the
    // `result_fallback`/`unsync_reads` TTL-presence checks (#149).

    // When a `create` block is supplied, reject the store-builder attributes the
    // macro would otherwise apply before the store-type match below. Without this
    // the match falls through to the generic "cache types are mutually exclusive"
    // arm (e.g. for `ttl_millis`), masking the specific conflict (#149).
    if args.create.is_some()
        && let Err(error) = check_create_conflicts(&args, fn_ident.span())
    {
        return error.to_compile_error().into();
    }

    // make the cache type and create statement
    let (cache_ty, cache_create) = if args.expires {
        if let Some(size) = args.max_size {
            (
                quote! { #krate::ExpiringLruCache<#cache_key_ty, #cache_value_ty> },
                quote! { #krate::ExpiringLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ExpiringLruCache build failed in #[cached]: {e}")) },
            )
        } else {
            (
                quote! { #krate::ExpiringCache<#cache_key_ty, #cache_value_ty> },
                quote! { #krate::ExpiringCache::builder().build().unwrap_or_else(|e| panic!("ExpiringCache build failed in #[cached]: {e}")) },
            )
        }
    } else {
        match (
            &args.max_size,
            has_ttl,
            &args.ty,
            &args.create,
            &args.refresh,
        ) {
            (Some(size), false, None, None, _) => {
                let cache_ty = quote! {#krate::LruCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {#krate::LruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("LruCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (None, true, None, None, refresh) => {
                let ttl_dur = ttl_duration.as_ref().expect("has_ttl implies ttl_duration");
                let cache_ty = quote! {#krate::TtlCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {#krate::TtlCache::builder().ttl(#ttl_dur).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("TtlCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (Some(size), true, None, None, refresh) => {
                let ttl_dur = ttl_duration.as_ref().expect("has_ttl implies ttl_duration");
                let cache_ty = quote! {#krate::LruTtlCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {#krate::LruTtlCache::builder().max_size(#size).ttl(#ttl_dur).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("LruTtlCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (None, false, None, None, _) => {
                let cache_ty = quote! {#krate::UnboundCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {#krate::UnboundCache::builder().build().unwrap_or_else(|e| panic!("UnboundCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (None, false, Some(type_str), Some(create_expr), _) => {
                let ty = match parse_str::<Type>(type_str) {
                    Ok(ty) => ty,
                    Err(error) => {
                        return syn::Error::new(
                            fn_ident.span(),
                            format!("unable to parse cache type: {error}"),
                        )
                        .to_compile_error()
                        .into();
                    }
                };

                let cache_create = expr_value_tokens(create_expr);

                (quote! { #ty }, cache_create)
            }
            (None, false, Some(_), None, _) => {
                return syn::Error::new(fn_ident.span(), "`ty` requires `create` to also be set")
                    .to_compile_error()
                    .into();
            }
            (None, false, None, Some(_), _) => {
                return syn::Error::new(fn_ident.span(), "`create` requires `ty` to also be set")
                    .to_compile_error()
                    .into();
            }
            _ => {
                return syn::Error::new(
                fn_ident.span(),
                "cache types (`max_size` and/or `ttl`, or `ty` and `create`) are mutually exclusive",
            )
            .to_compile_error()
            .into();
            }
        }
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (is_smart_result, is_smart_option) {
        (false, false) => {
            let set_cache_block =
                quote! { __cached_cache.cache_set(__cached_key, __cached_result.clone()); };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.to_owned(); __cached_r.set_was_cached(true); return __cached_r }
            } else {
                quote! { return __cached_result.to_owned() }
            };
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = quote! {
                if let Ok(__cached_inner) = &__cached_result {
                    __cached_cache.cache_set(__cached_key, __cached_inner.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.to_owned(); __cached_r.set_was_cached(true); return Ok(__cached_r) }
            } else {
                quote! { return Ok(__cached_result.to_owned()) }
            };
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = quote! {
                if let Some(__cached_inner) = &__cached_result {
                    __cached_cache.cache_set(__cached_key, __cached_inner.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.to_owned(); __cached_r.set_was_cached(true); return Some(__cached_r) }
            } else {
                quote! { return Some(__cached_result.to_owned()) }
            };
            (set_cache_block, return_cache_block)
        }
        (true, true) => unreachable!("return type cannot be both Result and Option"),
    };

    if let Err(error) = validate_sync_writes_buckets(args.sync_writes_buckets, fn_ident.span()) {
        return error.to_compile_error().into();
    }

    // `result_fallback` is only mutually exclusive with EXPLICITLY set non-Disabled
    // `sync_writes`. When `sync_writes` was not specified (unspecified, defaulting to
    // `ByKey`), `result_fallback` implicitly selects `Disabled` instead (per spec).
    // This lets `#[cached(result_fallback = true, ttl_secs = N)]` compile without
    // needing the user to also write `sync_writes = false`.
    if args.result_fallback && sync_writes_explicit && sync_writes != SyncWriteMode::Disabled {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` and `sync_writes` are mutually exclusive",
        )
        .to_compile_error()
        .into();
    }

    // When `result_fallback` is set and `sync_writes` was not explicitly specified,
    // override the default-ByKey to Disabled (result_fallback and ByKey are also
    // mutually exclusive, but we silently resolve the conflict for the unspecified case
    // rather than erroring).
    let sync_writes = if args.result_fallback && !sync_writes_explicit {
        SyncWriteMode::Disabled
    } else {
        sync_writes
    };

    if args.result_fallback && !is_result_return {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a `Result<T, E>` return type",
        )
        .to_compile_error()
        .into();
    }

    if args.result_fallback && args.ty.is_none() && !has_ttl && !args.expires {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a store that implements `CloneCached`. \
             The default `UnboundCache` and `LruCache` (size without ttl) do not implement it. \
             Use `ttl`/`ttl_secs`/`ttl_millis` (for `TtlCache`), `max_size` + a TTL \
             (for `LruTtlCache`), `expires` (for `ExpiringCache`/`ExpiringLruCache`), or a custom `ty`.",
        )
        .to_compile_error()
        .into();
    }

    if args.unsync_reads && matches!(args.sync_lock, Some(SyncLock::Mutex)) {
        return syn::Error::new(
            fn_ident.span(),
            "`unsync_reads` requires an RwLock; remove `sync_lock = \"mutex\"`",
        )
        .to_compile_error()
        .into();
    }

    if args.unsync_reads && args.ty.is_none() && (args.max_size.is_some() || has_ttl) {
        return syn::Error::new(
            fn_ident.span(),
            "`unsync_reads` requires a store that implements `CachedRead` (no mutation on reads). \
             `LruCache` and `LruTtlCache` update LRU recency on reads; `TtlCache` may refresh TTL. \
             Use the default store (UnboundCache), `TtlSortedCache`, or a custom `ty` that implements `CachedRead`.",
        )
        .to_compile_error()
        .into();
    }

    let sync_writes_buckets = args.sync_writes_buckets;

    let set_cache_and_return = quote! {
        #set_cache_block
        __cached_result
    };

    let use_rwlock = match args.sync_lock {
        Some(SyncLock::RwLock) => true,
        Some(SyncLock::Mutex) => false,
        None => true, // Default to RwLock for all caches now that traits support &self reads
    };

    let lock_type = if use_rwlock {
        if asyncness.is_some() {
            quote! { #krate::async_sync::RwLock }
        } else {
            quote! { #krate::sync_sync::RwLock }
        }
    } else {
        if asyncness.is_some() {
            quote! { #krate::async_sync::Mutex }
        } else {
            quote! { #krate::sync_sync::Mutex }
        }
    };

    let lock_method = if use_rwlock {
        quote! { write }
    } else {
        quote! { lock }
    };
    let read_lock_method = if use_rwlock {
        quote! { read }
    } else {
        quote! { lock }
    };
    let await_if_async = if asyncness.is_some() {
        quote! { .await }
    } else {
        quote! {}
    };

    let no_cache_fn_ident = Ident::new(&format!("{}_no_cache", &fn_ident), fn_ident.span());

    // When the cached fn is a method (`in_impl`), the origin/no-cache fn is also
    // a method on the same impl, so it must be invoked as `self.NAME_no_cache(...)`
    // rather than the free-fn `NAME_no_cache(...)` (#16/#140).
    let self_prefix = if has_receiver {
        quote! { self. }
    } else {
        quote! {}
    };

    // Build the origin ("no cache") function by cloning the full original
    // signature and renaming it. Quoting the whole `syn::Signature` (rather
    // than rebuilding it as `#generics (#inputs) #output`) preserves the
    // `where` clause, lifetimes, const generics, and `const`/`unsafe`/`abi`:
    // `#generics` alone emits only the angle-bracketed params and silently
    // drops the where clause.
    let mut no_cache_sig = signature.clone();
    no_cache_sig.ident = no_cache_fn_ident.clone();

    let function_no_cache;
    let function_call;
    // Build the cache static with a caller-supplied leading visibility token. The
    // module-scope static keeps the method's `#visibility`, but the `in_impl`
    // function-local static is emitted bare (no visibility): a visibility on a
    // function-local item is meaningless and trips `unreachable_pub` (#7).
    let make_static = |vis: &proc_macro2::TokenStream| match sync_writes {
        SyncWriteMode::ByKey => quote! {
            #vis static #cache_ident: ::std::sync::LazyLock<(#lock_type<#cache_ty>, Vec<std::sync::Arc<#lock_type<()>>>)> = ::std::sync::LazyLock::new(|| (#lock_type::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(#lock_type::new(()))).collect()));
        },
        _ => quote! {
            #vis static #cache_ident: ::std::sync::LazyLock<#lock_type<#cache_ty>> = ::std::sync::LazyLock::new(|| #lock_type::new(#cache_create));
        },
    };
    let module_ty = make_static(&quote! { #visibility });
    let body_ty = make_static(&quote! {});
    if asyncness.is_some() {
        function_no_cache = quote! {
            #no_cache_sig #body
        };

        function_call = quote! {
            let __cached_result = #self_prefix #no_cache_fn_ident(#(#input_names),*).await;
        };
    } else {
        function_no_cache = quote! {
            #no_cache_sig #body
        };

        function_call = quote! {
            let __cached_result = #self_prefix #no_cache_fn_ident(#(#input_names),*);
        };
    }

    // `force_refresh`: an opt-in boolean expression block over the fn args,
    // written in curly braces like `convert` (e.g. `force_refresh = "{ id == 0 }"`).
    // When it evaluates `true`, the cached-hit early return is skipped so the body
    // re-runs and re-caches. `if !(block)` guards each hit return; with no
    // `force_refresh` the guard is `if true` (always take the cached value).
    // This is orthogonal to `refresh` (TTL renewal on hit) (#146).
    let force_refresh_guard = match build_force_refresh_guard(&args.force_refresh, fn_ident.span())
    {
        Ok(guard) => guard,
        Err(error) => return error.to_compile_error().into(),
    };

    let (lock, do_set_return_block) = {
        let lock = match sync_writes {
            SyncWriteMode::ByKey => {
                let key_lock_block = by_key_lock_block(
                    quote! { __cached_key },
                    quote! { __cached_locks },
                    lock_method.clone(),
                    await_if_async.clone(),
                );
                quote! {
                    let (__cached_cache_mutex, __cached_locks) = &*#cache_ident;
                    #key_lock_block
                    let mut __cached_cache = __cached_cache_mutex.#lock_method()#await_if_async;
                }
            }
            _ => quote! {
                let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
            },
        };

        // The `#force_refresh_guard` wraps the whole lookup (not just the
        // early-return) so the `cache_get`/`cache_get_read` call is skipped when
        // force-refreshing. On a `refresh_on_hit` TTL store, `cache_get` renews
        // the entry's TTL as a side effect, which must not happen for a bypassed
        // entry (#146). Locking stays outside the guard (it does not renew TTL).
        let cache_get_return_block = if args.unsync_reads {
            quote! {
                let __cached_cache = #cache_ident.#read_lock_method()#await_if_async;
                #force_refresh_guard {
                    if let Some(__cached_result) = #krate::CachedRead::cache_get_read(&*__cached_cache, &__cached_key) {
                        #return_cache_block
                    }
                }
            }
        } else {
            quote! {
                let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                #force_refresh_guard {
                    if let Some(__cached_result) = __cached_cache.cache_get(&__cached_key) {
                        #return_cache_block
                    }
                }
            }
        };

        let by_key_cache_get_return_block = if args.unsync_reads {
            quote! {
                let __cached_cache = __cached_cache_mutex.#read_lock_method()#await_if_async;
                #force_refresh_guard {
                    if let Some(__cached_result) = #krate::CachedRead::cache_get_read(&*__cached_cache, &__cached_key) {
                        #return_cache_block
                    }
                }
            }
        } else {
            quote! {
                let mut __cached_cache = __cached_cache_mutex.#lock_method()#await_if_async;
                #force_refresh_guard {
                    if let Some(__cached_result) = __cached_cache.cache_get(&__cached_key) {
                        #return_cache_block
                    }
                }
            }
        };

        let do_set_return_block = match sync_writes {
            SyncWriteMode::ByKey => {
                let key_lock_block = by_key_lock_block(
                    quote! { __cached_key },
                    quote! { __cached_locks },
                    lock_method.clone(),
                    await_if_async.clone(),
                );
                quote! {
                    let (__cached_cache_mutex, __cached_locks) = &*#cache_ident;
                    #key_lock_block
                    {
                        #by_key_cache_get_return_block
                    }
                    #function_call
                    let mut __cached_cache = __cached_cache_mutex.#lock_method()#await_if_async;
                    #set_cache_and_return
                }
            }
            SyncWriteMode::Default => {
                if args.unsync_reads {
                    // When `force_refresh` IS set, hoist its predicate into a single
                    // boolean binding so it is evaluated AT MOST ONCE per call. Without
                    // this, the predicate would be expanded inside the optimistic
                    // read-lock block AND again in the write-lock re-check below,
                    // double-evaluating any side-effects in the user's predicate block.
                    //
                    // `#force_refresh_guard { false } else { true }` is
                    // `if !(block) { false } else { true }` == `block`, so the binding
                    // holds the user's predicate value.
                    //
                    // When `force_refresh` is absent, emit NEITHER the binding nor a read
                    // of it: the two read sites below fall back to `#force_refresh_guard`,
                    // which is `if true` with no `force_refresh`, so the cached value is
                    // always taken (equivalent to `if !__cached_force_refreshing` when the
                    // flag would be `false`). This avoids emitting a constant
                    // `if true { false } else { true }` binding (a needless-bool smell).
                    let (force_refreshing_flag, read_guard) = if args.force_refresh.is_some() {
                        (
                            quote! {
                                let __cached_force_refreshing = #force_refresh_guard { false } else { true };
                            },
                            quote! { if !__cached_force_refreshing },
                        )
                    } else {
                        (quote! {}, force_refresh_guard.clone())
                    };
                    let unsync_read_block = quote! {
                        let __cached_cache = #cache_ident.#read_lock_method()#await_if_async;
                        #read_guard {
                            if #krate::CachedPeek::cache_peek(&*__cached_cache, &__cached_key).is_some() {
                                if let Some(__cached_result) = #krate::CachedRead::cache_get_read(&*__cached_cache, &__cached_key) {
                                    #return_cache_block
                                }
                            }
                        }
                    };
                    quote! {
                        #force_refreshing_flag
                        {
                            #unsync_read_block
                        }
                        let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                        #read_guard {
                            if let Some(__cached_result) = __cached_cache.cache_get(&__cached_key) {
                                #return_cache_block
                            }
                        }
                        #function_call
                        #set_cache_and_return
                    }
                } else {
                    quote! {
                        let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                        #force_refresh_guard {
                            if let Some(__cached_result) = __cached_cache.cache_get(&__cached_key) {
                                #return_cache_block
                            }
                        }
                        #function_call
                        #set_cache_and_return
                    }
                }
            }
            SyncWriteMode::Disabled => {
                if args.result_fallback {
                    // Capture the prior `Ok` value to fall back to when the refresh
                    // returns `Err`. The renewing `cache_get_with_expiry_status`
                    // (LRU promotion, hit-count, possible TTL renewal on `refresh`)
                    // serves the genuine early-return on a fresh hit. When
                    // `force_refresh` bypasses the entry, that renewing read must NOT
                    // run (#146): a bypassed entry must have no read side effects.
                    // `#force_refresh_guard` is `if !(block)` (taken when NOT
                    // bypassing); on the bypass path capture the fallback value with a
                    // non-renewing `cache_peek_with_expiry_status` (no promote/hit-count/
                    // TTL-renew), which also returns expired entries (unlike `cache_peek`
                    // which returns `None` for expired entries, losing the stale fallback).
                    // With no `force_refresh` the guard is `if true`, so the peek arm is
                    // dead and behavior is unchanged.
                    let capture_old_val = if args.force_refresh.is_some() {
                        quote! {
                            if __cached_force_refreshing {
                                // Bypassed: peek without renewing/promoting/hit-counting.
                                // Also captures expired entries so an Err recompute over
                                // an expired entry still returns the stale Ok fallback.
                                let (__cached_peek_val, _) = #krate::CloneCached::cache_peek_with_expiry_status(&*__cached_cache, &__cached_key);
                                __cached_old_val = __cached_peek_val;
                            } else {
                                let (__cached_result, __cached_has_expired) = __cached_cache.cache_get_with_expiry_status(&__cached_key);
                                if let (Some(__cached_result), false) = (&__cached_result, __cached_has_expired) {
                                    // Not bypassing (guard always taken here), so the
                                    // early-return is unconditional on a fresh hit.
                                    #return_cache_block
                                }
                                __cached_old_val = __cached_result;
                            }
                        }
                    } else {
                        quote! {
                            let (__cached_result, __cached_has_expired) = __cached_cache.cache_get_with_expiry_status(&__cached_key);
                            if let (Some(__cached_result), false) = (&__cached_result, __cached_has_expired) {
                                #force_refresh_guard {
                                    #return_cache_block
                                }
                            }
                            __cached_old_val = __cached_result;
                        }
                    };
                    // Evaluate the `force_refresh` predicate once: `#force_refresh_guard`
                    // is `if !(block)`, so `if !(block) { false } else { true }` == `block`.
                    let force_refreshing_flag = if args.force_refresh.is_some() {
                        quote! { let __cached_force_refreshing = #force_refresh_guard { false } else { true }; }
                    } else {
                        quote! {}
                    };
                    quote! {
                        #force_refreshing_flag
                        let __cached_old_val = {
                            let mut __cached_old_val = None;
                            let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                            #capture_old_val
                            __cached_old_val
                        };
                        #function_call
                        let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                        let __cached_result = match (__cached_result.is_err(), __cached_old_val) {
                            (true, Some(__cached_old_val)) => {
                                Ok(__cached_old_val)
                            }
                            _ => __cached_result
                        };
                        #set_cache_and_return
                    }
                } else {
                    quote! {
                        {
                            #cache_get_return_block
                        }
                        #function_call
                        let mut __cached_cache = #cache_ident.#lock_method()#await_if_async;
                        #set_cache_and_return
                    }
                }
            }
        };
        (lock, do_set_return_block)
    };

    let signature_no_muts = get_mut_signature(signature);

    // create a signature for the cache-priming function
    let prime_fn_ident = Ident::new(&format!("{}_prime_cache", &fn_ident), fn_ident.span());
    let mut prime_sig = signature_no_muts.clone();
    prime_sig.ident = prime_fn_ident;

    // make cached static, cached function and prime cached function doc comments
    let cache_ident_doc = format!("Cached static for the [`{}`] function.", fn_ident);
    let no_cache_fn_indent_doc = format!("Origin of the cached function [`{}`].", fn_ident);
    let prime_fn_indent_doc = format!("Primes the cached function [`{}`].", fn_ident);
    let cache_fn_doc_extra = if args.in_impl {
        "This is a cached method; its cache static is function-local to the method body."
            .to_string()
    } else {
        format!(
            "This is a cached function that uses the [`{}`] cached static.",
            cache_ident
        )
    };
    fill_in_attributes(&mut attributes, cache_fn_doc_extra);

    let prime_do_set_return_block = quote! {
        // try to get a lock first
        #lock
        // run the function and cache the result
        #function_call
        #set_cache_and_return
    };

    // When `in_impl`, the cache static cannot sit at impl scope (a `static` is
    // not a valid impl item), so it is emitted inside each generated fn body -
    // which is also valid Rust now (item-in-fn). This additionally fixes static
    // collisions between same-named methods on different types (#16/#140).
    // Off the `in_impl` path the static is emitted once at module scope.
    let (module_static, body_static) = if args.in_impl {
        // No `#[doc]`: a function-local static is not part of the public API and
        // rustdoc ignores doc attributes on it, so the doc string would be dead.
        // The function-local static is emitted bare (no visibility) - a meaningless
        // visibility on a function-local item trips `unreachable_pub` (#7).
        (quote! {}, quote! { #body_ty })
    } else {
        (
            quote! {
                #[doc = #cache_ident_doc]
                #module_ty
            },
            quote! {},
        )
    };

    // The cache static is function-local when `in_impl = true`, so the cached
    // method and a `{fn}_prime_cache` sibling would each get a distinct
    // function-local static - priming would populate a static the cached method
    // never reads (a silent no-op). A function-local static cannot be shared
    // between two sibling methods, so a correct prime is impossible under
    // `in_impl`; do not emit the companion at all. Calling a non-existent prime
    // fn is then a clear compile error instead of a silent no-op (#16/#140).
    let prime_fn = if args.in_impl {
        quote! {}
    } else {
        quote! {
            // Prime cached function. Priming is optional, so suppress
            // `dead_code` for callers that generate but never call the companion.
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #(#attributes)*
            #companions_visibility #prime_sig {
                #body_static
                use #krate::Cached;
                let __cached_key = #key_convert_block;
                #prime_do_set_return_block
            }
        }
    };

    // On the `in_impl` path the `{fn}_no_cache` origin is a public impl method, so
    // it would otherwise surface in consumers' rustdoc as unintended API. Hide it
    // with `#[doc(hidden)]` (it stays callable as an escape hatch). Off `in_impl`
    // it is a free fn that keeps its descriptive origin doc.
    let no_cache_fn_doc = if args.in_impl {
        quote! { #[doc(hidden)] }
    } else {
        quote! { #[doc = #no_cache_fn_indent_doc] }
    };

    // put it all together
    let expanded = quote! {
        // Cached static (module scope unless `in_impl`)
        #module_static
        // No cache function (origin of the cached function)
        #no_cache_fn_doc
        #companions_visibility #function_no_cache
        // Cached function
        #(#attributes)*
        #visibility #signature_no_muts {
            #body_static
            use #krate::Cached;
            use #krate::CloneCached;
            let __cached_key = #key_convert_block;
            #do_set_return_block
        }
        // Prime cached function (omitted for `in_impl` methods)
        #prime_fn
    };

    expanded.into()
}
