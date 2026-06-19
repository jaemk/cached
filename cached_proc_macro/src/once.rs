use crate::helpers::*;
use darling::FromMeta;
use darling::ast::NestedMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Ident, ItemFn, ReturnType, parse_macro_input, parse_str};

#[derive(FromMeta)]
struct OnceMacroArgs {
    #[darling(default)]
    name: Option<String>,
    /// An expiry expressed as a `Duration` expression in a string literal (same
    /// convention as `create`/`convert`), e.g.
    /// `ttl = "core::time::Duration::from_secs(60)"`. Mutually exclusive with
    /// `ttl_secs`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl: Option<TtlExpr>,
    /// Expiry in whole seconds. Convenience alternative to `ttl`. Mutually
    /// exclusive with `ttl`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl_secs: Option<u64>,
    /// Expiry in milliseconds. A finer-grained alternative to `ttl_secs`;
    /// mutually exclusive with `ttl`, `ttl_secs`, and `expires` (#149).
    #[darling(default)]
    ttl_millis: Option<u64>,
    #[darling(default)]
    time: Option<u64>,
    /// `None` = not specified by user (defaults to `Disabled` for `#[once]`).
    #[darling(default)]
    sync_writes: Option<SyncWriteMode>,
    #[darling(default = "default_sync_writes_buckets")]
    sync_writes_buckets: usize,
    #[darling(default)]
    cache_err: bool,
    #[darling(default)]
    cache_none: bool,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    expires: bool,
    /// Allow the macro on a method that takes `self` inside an `impl` block.
    /// Note: `#[once]` stores a single value for *all* receivers, so an
    /// `in_impl` `#[once]` method shares one cached value across every instance
    /// (#16/#140).
    #[darling(default)]
    in_impl: bool,
    /// Opt-in boolean expression over the fn args. Both unquoted `{ expr }` and
    /// legacy quoted `"{ expr }"` forms are accepted. When it evaluates `true`, the
    /// single cached value is bypassed and the body re-runs and re-caches.
    /// `#[once]` has no per-call key, so unlike `#[cached]` there is no "exclude
    /// the flag from the key" caveat: a forced recompute overwrites the one shared
    /// value for all callers.
    #[darling(default)]
    force_refresh: Option<syn::Expr>,
    /// Override the visibility of the companion fns (`{fn}_no_cache`,
    /// `{fn}_prime_cache`). `None` (default) inherits the cached fn's visibility.
    #[darling(default)]
    companions_vis: Option<String>,
    // Removed attributes intercepted to provide helpful error messages
    #[darling(default)]
    result: Option<bool>,
    #[darling(default)]
    option: Option<bool>,
    // `#[cached]`-only attributes - intercepted to provide a clear error instead
    // of darling's generic "unknown field" message.
    #[darling(default)]
    sync_lock: Option<String>,
    #[darling(default)]
    unsync_reads: Option<bool>,
    #[darling(default)]
    result_fallback: Option<bool>,
    #[darling(default)]
    refresh: Option<bool>,
    #[darling(default)]
    max_size: Option<usize>,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<String>,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<syn::Expr>,
}

fn default_sync_writes_buckets() -> usize {
    64
}

pub fn once(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    let args = match OnceMacroArgs::from_list(&attr_args) {
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

    // Resolve the path to the `cached` crate (renamed-dependency support, #157).
    let krate = crate_path();

    // Resolve the effective sync_writes mode.
    // `None` (unspecified by user) defaults to `Disabled` for `#[once]`.
    // `#[once]` never changes its default based on other attrs.
    let sync_writes = args.sync_writes.unwrap_or(SyncWriteMode::Disabled);

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness;
    let has_receiver = inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Receiver(_)));

    // Reject `self` methods unless `in_impl = true` (#[once] has no `convert`).
    if has_receiver && !args.in_impl {
        return syn::Error::new(
            fn_ident.span(),
            "#[once] cannot be applied to methods that take `self`. \
             Use `in_impl = true` to cache a method inside an `impl` block. \
             Note: `#[once]` stores a single value shared across all instances.",
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

    // Note: `#[once]` supports generic functions. Its static only holds the
    // (concrete) value type, never the function's type parameters, so no
    // generic-rejection check is needed here (unlike `#[cached]` /
    // `#[concurrent_cached]`, whose key/value types can leak generics) (#80).

    if args.time.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time` (whole seconds) was renamed in cached 1.0; use `ttl_secs = ...` \
             (or `ttl = \"Duration::from_secs(...)\"` / `ttl_millis = ...`)",
        )
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
             `ttl_secs` applies a uniform time-based TTL",
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
             `ttl` applies a uniform time-based TTL",
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

    if args.sync_lock.is_some() {
        return syn::Error::new(fn_ident.span(), "`sync_lock` is not supported on `#[once]`")
            .to_compile_error()
            .into();
    }

    if args.unsync_reads.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`unsync_reads` is not supported on `#[once]`",
        )
        .to_compile_error()
        .into();
    }

    // Reject the remaining `#[cached]`-only attributes. `#[once]` stores a single
    // shared value (not a keyed map), so these store-shaping / keying attributes do
    // not apply. Intercept each with a friendly message instead of darling's generic
    // "unknown field" error (mirrors `reject_cached_only_attrs` in
    // `concurrent_cached.rs`).
    if args.result_fallback.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` is not supported on `#[once]`; \
             it returns the last cached `Ok` value from a keyed cache, but `#[once]` stores a \
             single value and already returns the one cached `Ok` on subsequent calls",
        )
        .to_compile_error()
        .into();
    }
    if args.refresh.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`refresh` is not supported on `#[once]`; \
             `refresh` renews a per-entry TTL on cache hit, but `#[once]` stores a single value \
             and does not refresh on read - set `ttl`/`ttl_secs`/`ttl_millis` for time-based expiry",
        )
        .to_compile_error()
        .into();
    }
    if args.max_size.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`max_size` is not supported on `#[once]`; \
             `#[once]` stores a single value, so there is no entry count to bound",
        )
        .to_compile_error()
        .into();
    }
    if args.ty.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`ty` is not supported on `#[once]`; \
             `#[once]` manages its own single-value storage and does not take a custom store type",
        )
        .to_compile_error()
        .into();
    }
    if args.create.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`create` is not supported on `#[once]`; \
             `#[once]` manages its own single-value storage and does not take a custom store \
             constructor",
        )
        .to_compile_error()
        .into();
    }
    if args.key.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`key` is not supported on `#[once]`; \
             `#[once]` stores a single value for all arguments and has no per-call cache key",
        )
        .to_compile_error()
        .into();
    }
    if args.convert.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`convert` is not supported on `#[once]`; \
             `#[once]` stores a single value for all arguments and has no per-call cache key to convert",
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

    // pull out the names and types of the function inputs
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
            Ident::new(name, fn_ident.span())
        }
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    if let Err(error) = validate_sync_writes_buckets(args.sync_writes_buckets, fn_ident.span()) {
        return error.to_compile_error().into();
    }
    if sync_writes == SyncWriteMode::ByKey {
        return syn::Error::new(
            fn_ident.span(),
            "`sync_writes = \"by_key\"` is not supported by `#[once]` because `#[once]` stores a single value for all arguments",
        )
        .to_compile_error()
        .into();
    }
    let sync_writes_buckets = args.sync_writes_buckets;

    // `has_ttl` / `ttl_duration` were resolved above (from `ttl` expr, `ttl_secs`,
    // or `ttl_millis`); `has_ttl` gates the timestamped storage shape (#149).

    // make the cache type and create statement
    let (cache_ty, cache_create) = if has_ttl {
        (
            quote! { Option<(#krate::time::Instant, #cache_value_ty)> },
            quote! { None },
        )
    } else {
        (quote! { Option<#cache_value_ty> }, quote! { None })
    };

    // `force_refresh`: when its expression evaluates `true`, the cached-hit early
    // return is skipped so the body re-runs and re-caches the single shared value.
    // The guard wraps the whole cached-value check (not just the return), so a
    // TTL'd entry's expiry test is bypassed too and the body always re-runs.
    let force_refresh_guard = match build_force_refresh_guard(&args.force_refresh, fn_ident.span())
    {
        Ok(guard) => guard,
        Err(error) => return error.to_compile_error().into(),
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (is_smart_result, is_smart_option) {
        (false, false) => {
            let set_cache_block = if has_ttl {
                quote! {
                    *__cached_cached = Some((#krate::time::Instant::now(), __cached_result.clone()));
                }
            } else {
                quote! {
                    *__cached_cached = Some(__cached_result.clone());
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.clone(); __cached_r.was_cached = true; return __cached_r }
            } else {
                quote! { return __cached_result.clone() }
            };
            let return_cache_block = gen_return_cache_block(
                &krate,
                ttl_duration.clone(),
                args.expires,
                return_cache_block,
            );
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = if has_ttl {
                quote! {
                    if let Ok(__cached_inner) = &__cached_result {
                        *__cached_cached = Some((#krate::time::Instant::now(), __cached_inner.clone()));
                    }
                }
            } else {
                quote! {
                    if let Ok(__cached_inner) = &__cached_result {
                        *__cached_cached = Some(__cached_inner.clone());
                    }
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.clone(); __cached_r.was_cached = true; return Ok(__cached_r) }
            } else {
                quote! { return Ok(__cached_result.clone()) }
            };
            let return_cache_block = gen_return_cache_block(
                &krate,
                ttl_duration.clone(),
                args.expires,
                return_cache_block,
            );
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = if has_ttl {
                quote! {
                    if let Some(__cached_inner) = &__cached_result {
                        *__cached_cached = Some((#krate::time::Instant::now(), __cached_inner.clone()));
                    }
                }
            } else {
                quote! {
                    if let Some(__cached_inner) = &__cached_result {
                        *__cached_cached = Some(__cached_inner.clone());
                    }
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut __cached_r = __cached_result.clone(); __cached_r.was_cached = true; return Some(__cached_r) }
            } else {
                quote! { return Some(__cached_result.clone()) }
            };
            let return_cache_block = gen_return_cache_block(
                &krate,
                ttl_duration.clone(),
                args.expires,
                return_cache_block,
            );
            (set_cache_block, return_cache_block)
        }
        (true, true) => unreachable!("return type cannot be both Result and Option"),
    };

    let set_cache_and_return = quote! {
        #set_cache_block
        __cached_result
    };

    // Clone the full original signature and rename it to `<fn>_no_cache`. Quoting
    // the whole `syn::Signature` preserves the `where` clause (and lifetimes,
    // const generics, etc.) - `#generics` alone drops the where clause.
    // Unique per-function name so multiple `in_impl` methods on the same impl
    // block do not collide on a shared `<fn>_no_cache` sibling method.
    let inner_fn_ident = Ident::new(&format!("{}_no_cache", &fn_ident), fn_ident.span());
    let mut inner_sig = signature.clone();
    inner_sig.ident = inner_fn_ident.clone();

    // For `in_impl` methods the body may reference `self`, so `<fn>_no_cache`
    // must be a sibling impl method (a nested fn cannot capture `self`); it is
    // invoked as `self.<fn>_no_cache(...)`. For free functions it stays a nested
    // fn defined inline in the body (#16/#140).
    let self_prefix = if has_receiver {
        quote! { self. }
    } else {
        quote! {}
    };
    // The `in_impl` origin sibling is a public impl method; hide it from consumers'
    // rustdoc with `#[doc(hidden)]` (it stays callable as an escape hatch).
    // Resolve companion fn visibility (#9). Needs to come before inner_sibling_def.
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

    let (inner_sibling_def, inner_nested_def) = if args.in_impl {
        (
            quote! { #[doc(hidden)] #companions_visibility #inner_sig #body },
            quote! {},
        )
    } else {
        (quote! {}, quote! { #inner_sig #body })
    };

    let r_lock;
    let w_lock;
    let function_call;
    // Build the cache static with a caller-supplied leading visibility token. The
    // module-scope static keeps the method's `#visibility`, but the `in_impl`
    // function-local static is emitted bare (no visibility): a visibility on a
    // function-local item is meaningless and trips `unreachable_pub` (#7).
    let make_static: Box<dyn Fn(&proc_macro2::TokenStream) -> proc_macro2::TokenStream>;
    if asyncness.is_some() {
        w_lock = quote! {
            // try to get a write lock
            let mut __cached_cached = #cache_ident.write().await;
        };

        r_lock = quote! {
            // try to get a read lock
            let __cached_cached = #cache_ident.read().await;
        };

        function_call = quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*).await;
        };

        let cache_ident = cache_ident.clone();
        let cache_ty = cache_ty.clone();
        let cache_create = cache_create.clone();
        let krate = krate.clone();
        let is_by_key = sync_writes == SyncWriteMode::ByKey;
        make_static = Box::new(move |vis: &proc_macro2::TokenStream| {
            if is_by_key {
                quote! {
                    #vis static #cache_ident: ::std::sync::LazyLock<(#krate::async_sync::RwLock<#cache_ty>, Vec<std::sync::Arc<#krate::async_sync::RwLock<()>>>)> = ::std::sync::LazyLock::new(|| (#krate::async_sync::RwLock::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(#krate::async_sync::RwLock::new(()))).collect()));
                }
            } else {
                quote! {
                    #vis static #cache_ident: ::std::sync::LazyLock<#krate::async_sync::RwLock<#cache_ty>> = ::std::sync::LazyLock::new(|| #krate::async_sync::RwLock::new(#cache_create));
                }
            }
        });
    } else {
        w_lock = quote! {
            // try to get a lock first
            let mut __cached_cached = #cache_ident.write();
        };

        r_lock = quote! {
            // try to get a read lock
            let __cached_cached = #cache_ident.read();
        };

        function_call = quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*);
        };

        let cache_ident = cache_ident.clone();
        let cache_ty = cache_ty.clone();
        let cache_create = cache_create.clone();
        let krate = krate.clone();
        let is_by_key = sync_writes == SyncWriteMode::ByKey;
        make_static = Box::new(move |vis: &proc_macro2::TokenStream| {
            if is_by_key {
                quote! {
                    #vis static #cache_ident: ::std::sync::LazyLock<(#krate::sync_sync::RwLock<#cache_ty>, Vec<std::sync::Arc<#krate::sync_sync::RwLock<()>>>)> = ::std::sync::LazyLock::new(|| (#krate::sync_sync::RwLock::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(#krate::sync_sync::RwLock::new(()))).collect()));
                }
            } else {
                quote! {
                    #vis static #cache_ident: ::std::sync::LazyLock<#krate::sync_sync::RwLock<#cache_ty>> = ::std::sync::LazyLock::new(|| #krate::sync_sync::RwLock::new(#cache_create));
                }
            }
        });
    }
    let module_ty = make_static(&quote! { #visibility });
    let body_ty = make_static(&quote! {});

    let prime_do_set_return_block = match sync_writes {
        SyncWriteMode::ByKey => unreachable!("ByKey rejected above"),
        _ => quote! {
            #w_lock
            #function_call
            #set_cache_and_return
        },
    };

    let r_lock_return_cache_block = quote! {
        {
            #r_lock
            #force_refresh_guard {
                if let Some(__cached_result) = &*__cached_cached {
                    #return_cache_block
                }
            }
        }
    };

    let do_set_return_block = match sync_writes {
        SyncWriteMode::Default => {
            // When `force_refresh` IS set, hoist its predicate into a single
            // boolean so it is evaluated ONCE per call. Without this the
            // predicate would be expanded both inside the optimistic read-lock
            // block and again in the write-lock re-check below, double-evaluating
            // any side-effects in the user's predicate block (#FIX-B).
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
            // Inline read-lock block using the already-computed guard so the
            // predicate is not re-evaluated here.
            let r_lock_return_cache_block_hoisted = quote! {
                {
                    #r_lock
                    #read_guard {
                        if let Some(__cached_result) = &*__cached_cached {
                            #return_cache_block
                        }
                    }
                }
            };
            quote! {
                #force_refreshing_flag
                #r_lock_return_cache_block_hoisted
                #w_lock
                #read_guard {
                    if let Some(__cached_result) = &*__cached_cached {
                        #return_cache_block
                    }
                }
                #function_call
                #set_cache_and_return
            }
        }
        SyncWriteMode::ByKey => unreachable!("ByKey rejected above"),
        SyncWriteMode::Disabled => quote! {
            #r_lock_return_cache_block
            #function_call
            #w_lock
            #set_cache_and_return
        },
    };

    let signature_no_muts = get_mut_signature(signature);

    let prime_fn_ident = Ident::new(&format!("{}_prime_cache", &fn_ident), fn_ident.span());
    let mut prime_sig = signature_no_muts.clone();
    prime_sig.ident = prime_fn_ident;

    // make cached static, cached function and prime cached function doc comments
    let cache_ident_doc = format!("Cached static for the [`{}`] function.", fn_ident);
    let prime_fn_indent_doc = format!("Primes the cached function [`{}`].", fn_ident);
    let cache_fn_doc_extra = format!(
        "This is a cached function that uses the [`{}`] cached static.",
        cache_ident
    );
    fill_in_attributes(&mut attributes, cache_fn_doc_extra);

    // put it all together
    let now_block = if has_ttl {
        quote! { let __cached_now = #krate::time::Instant::now(); }
    } else {
        quote! {}
    };

    // The cache static cannot sit at impl scope when `in_impl`; emit it inside
    // each generated fn body instead (also fixes same-named-method collisions).
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
            #companions_visibility #prime_sig {
                #body_static
                #now_block
                #prime_do_set_return_block
            }
        }
    };

    let expanded = quote! {
        // Cached static (module scope unless `in_impl`)
        #module_static
        // Inner origin fn as a sibling impl method (only when `in_impl`)
        #inner_sibling_def
        // Cached function
        #(#attributes)*
        #visibility #signature_no_muts {
            #body_static
            #now_block
            #do_set_return_block
        }
        // Prime cached function (omitted for `in_impl` methods)
        #prime_fn
    };

    expanded.into()
}
