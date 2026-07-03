use crate::helpers::*;
use darling::FromMeta;
use darling::ast::NestedMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{GenericArgument, Ident, ItemFn, ReturnType, Type, parse_macro_input, parse_str};

#[derive(FromMeta)]
struct ConcurrentCachedArgs {
    #[darling(default)]
    map_error: Option<syn::Expr>,
    #[darling(default)]
    disk: bool,
    #[darling(default)]
    disk_dir: Option<String>,
    #[darling(default)]
    redis: bool,
    #[darling(default)]
    cache_prefix_block: Option<syn::Expr>,
    #[darling(default)]
    name: Option<String>,
    /// A TTL expressed as a `Duration` expression in a string literal (same
    /// convention as `create`/`convert`), e.g.
    /// `ttl = "core::time::Duration::from_secs(60)"`. Mutually exclusive with
    /// `ttl_secs`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl: Option<TtlExpr>,
    /// TTL in whole seconds. Convenience alternative to `ttl`. Mutually
    /// exclusive with `ttl`, `ttl_millis`, and `expires`.
    #[darling(default)]
    ttl_secs: Option<u64>,
    /// TTL in milliseconds. A finer-grained alternative to `ttl_secs`;
    /// mutually exclusive with `ttl`, `ttl_secs`, and `expires` (#149).
    #[darling(default)]
    ttl_millis: Option<u64>,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    time_refresh: Option<bool>,
    /// Removed alias for `max_size`; kept only to emit a helpful rename error.
    #[darling(default)]
    size: Option<usize>,
    #[darling(default)]
    refresh: bool,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<syn::Expr>,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    cache_err: bool,
    #[darling(default)]
    cache_none: bool,
    /// When `true`, an `Err` return serves the last cached `Ok` value for that key.
    /// Requires `ttl`, `ttl_secs`, or `ttl_millis`. The stale value is read from the primary TTL cache slot via
    /// `ConcurrentCloneCached::cache_get_with_expiry_status` (no separate store is
    /// created) and re-cached with a fresh TTL window on `Err`.
    #[darling(default)]
    result_fallback: bool,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<syn::Expr>,
    #[darling(default)]
    durable: Option<bool>,
    /// Total LRU capacity for the default in-memory sharded store.
    /// Mirrors the `max_size` builder/constructor naming on the cache stores.
    /// Only meaningful when `redis=false`, `disk=false`, and `create` is not set.
    #[darling(default)]
    max_size: Option<usize>,
    /// Number of shards for the default in-memory sharded store.
    /// Only meaningful when `redis=false`, `disk=false`, and `create` is not set.
    #[darling(default)]
    shards: Option<usize>,
    #[darling(default)]
    expires: bool,
    /// A boolean expression over the function arguments; when `true`, the cached
    /// value is bypassed and the body is re-run and re-cached. Orthogonal to
    /// `refresh`. Both unquoted `{ expr }` and legacy quoted `"{ expr }"` forms
    /// are accepted (#146).
    #[darling(default)]
    force_refresh: Option<syn::Expr>,
    /// Override the visibility of the companion fns (`{fn}_no_cache`,
    /// `{fn}_prime_cache`). `None` (default) inherits the cached fn's visibility.
    #[darling(default)]
    companions_vis: Option<String>,
    /// Allow the macro on a method that takes `self` inside an `impl` block.
    /// The cache static is emitted inside the generated fn body and the receiver
    /// is preserved/forwarded (#16/#140).
    #[darling(default)]
    in_impl: bool,
}

/// When a `create` block is supplied the user fully constructs the store, so
/// every store-builder attribute the macro would otherwise apply is dropped.
/// Reject those attributes with a precise message instead of silently ignoring
/// them - otherwise `disk_dir` / `durable` (and `ttl` /
/// `refresh` / `cache_prefix_block`) look applied but are not.
fn check_create_conflicts(
    args: &ConcurrentCachedArgs,
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
    if args.cache_prefix_block.is_some() {
        conflicting.push("cache_prefix_block");
    }
    if args.disk_dir.is_some() {
        conflicting.push("disk_dir");
    }
    if args.durable.is_some() {
        conflicting.push("durable");
    }
    if args.max_size.is_some() {
        conflicting.push("max_size");
    }
    if args.shards.is_some() {
        conflicting.push("shards");
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

fn reject_cached_only_attrs(attr_args: &[NestedMeta]) -> Result<(), syn::Error> {
    for arg in attr_args {
        let Some(meta) = (match arg {
            NestedMeta::Meta(meta) => Some(meta),
            NestedMeta::Lit(_) => None,
        }) else {
            continue;
        };
        let Some(ident) = meta.path().get_ident().map(ToString::to_string) else {
            continue;
        };
        let message = match ident.as_str() {
            "result" => Some(
                "`result` is not a valid attribute for `#[concurrent_cached]`; \
                 return `Result<T, E>` and only `Ok` values are cached by default. \
                 Use `cache_err = true` to also cache `Err` values.",
            ),
            "option" => Some(
                "`option = true` is not a valid attribute for `#[concurrent_cached]`; \
                 `Option<T>` returns skip `None` by default. \
                 Use `cache_none = true` to force caching `None` values.",
            ),
            "sync_writes" => Some(
                "`sync_writes` is not supported on `#[concurrent_cached]`; concurrent stores \
                 synchronize cache access internally but do not deduplicate first-call execution",
            ),
            "sync_writes_buckets" => {
                Some("`sync_writes_buckets` is not supported on `#[concurrent_cached]`")
            }
            "sync_lock" => Some("`sync_lock` is not supported on `#[concurrent_cached]`"),
            "unsync_reads" => Some("`unsync_reads` is not supported on `#[concurrent_cached]`"),
            _ => None,
        };
        if let Some(message) = message {
            return Err(syn::Error::new(meta.span(), message));
        }
    }
    Ok(())
}

pub fn concurrent_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    if let Err(e) = reject_cached_only_attrs(&attr_args) {
        return e.to_compile_error().into();
    }
    let args = match ConcurrentCachedArgs::from_list(&attr_args) {
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

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness;
    let has_receiver = inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Receiver(_)));

    // Reject `self` methods unless `in_impl = true`. A `self` receiver only
    // exists inside an `impl`/trait, and off the `in_impl` path the cache static
    // is emitted at that same scope, where a `static` is not a valid item - so a
    // `convert` block alone cannot rescue a `self` method (it would still fail
    // later with an opaque error). `in_impl` is the only fix (#16/#140).
    if has_receiver && !args.in_impl {
        return syn::Error::new(
            fn_ident.span(),
            "#[concurrent_cached] cannot be applied to methods that take `self`. \
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

    // Generic functions need the cache key pinned to a concrete type via
    // `key` + `convert` (and a concrete store `ty`/`create`): the cache is a
    // single monomorphic static and cannot name the function's type parameters.
    // Without `convert` the default-key path embeds the type parameters in the
    // key type and cannot compile - reject it with a clear diagnostic (#80).
    if (signature.generics.type_params().next().is_some()
        || signature.generics.const_params().next().is_some())
        && args.convert.is_none()
    {
        return syn::Error::new(
            fn_ident.span(),
            "#[concurrent_cached] on a generic function requires `key` + `convert` to pin the cache \
             key to a concrete type: the cache is a single monomorphic static shared across all \
             instantiations and cannot name the function's type parameters. \
             Provide `key`/`convert` (and a concrete `ty`/`create`), or wrap the generic function \
             in a non-generic `#[concurrent_cached]` function per concrete type.",
        )
        .to_compile_error()
        .into();
    }

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

    if args.size.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`size` was renamed to `max_size`; use `max_size = ...`",
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
             `expires` delegates expiry to the value via the `Expires` trait",
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
            "`expires` and `ttl` are mutually exclusive - `expires` delegates expiry to the value via the `Expires` trait",
        )
        .to_compile_error()
        .into();
    }
    // Resolve the TTL `Duration` token from whichever of `ttl` (expr), `ttl_secs`,
    // or `ttl_millis` is set. This performs the 3-way mutual-exclusion check, the
    // `ttl_secs`/`ttl_millis` >= 1 validation, and parses the `ttl` expression.
    // (A zero `ttl_secs`/`ttl_millis` is rejected here, before any store path runs.)
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

    if args.expires {
        if args.redis {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `redis` are mutually exclusive - `expires` selects sharded in-memory expiring stores",
            )
            .to_compile_error()
            .into();
        }
        if args.disk {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `disk` are mutually exclusive - `expires` selects sharded in-memory expiring stores",
            )
            .to_compile_error()
            .into();
        }
        if args.ty.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `ty` are mutually exclusive - `expires` generates the store type automatically",
            )
            .to_compile_error()
            .into();
        }
        if args.create.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `create` are mutually exclusive - `expires` generates the store constructor automatically",
            )
            .to_compile_error()
            .into();
        }
        if args.refresh {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `refresh` are mutually exclusive - `expires` delegates expiry to the value via `Expires::is_expired`",
            )
            .to_compile_error()
            .into();
        }
        if args.cache_none {
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
        if args.result_fallback {
            return syn::Error::new(
                fn_ident.span(),
                "`result_fallback = true` and `expires = true` are mutually exclusive - \
                 `expires` selects a per-value expiry store; `result_fallback` requires \
                 a fixed-TTL store whose entry expiry can be detected and refreshed by \
                 the cache layer, which per-value expiry does not support. \
                 Note: `ttl` and `expires` serve different purposes - `ttl` applies a fixed \
                 TTL to all entries, while `expires` delegates expiry to each value. \
                 If you need time-based expiry together with `result_fallback`, use `ttl` \
                 (not `expires`).",
            )
            .to_compile_error()
            .into();
        }
        if args.cache_err {
            return syn::Error::new(
                fn_ident.span(),
                "`expires = true` and `cache_err = true` are mutually exclusive - `expires` \
                 requires the cached value to implement `Expires`, but `cache_err = true` \
                 stores `Result<T, E>` as the value type, which does not implement `Expires`. \
                 Remove `cache_err = true`.",
            )
            .to_compile_error()
            .into();
        }
    }

    let input_tys = get_input_types(&inputs);

    let input_names = get_input_names(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let output_span = output_ty.span();
    let output_ts = TokenStream::from(output_ty);
    let output_type_display = output_ts.to_string().replace(' ', "");

    // Detect return type shape. These drive smart-caching decisions throughout.
    let is_option_return = is_option_return_type(&output);
    let is_result_return = is_result_return_type(&output);

    // `is_smart_option`: skip None, cache Some(T) - default for Option<T> returns.
    // Opt out with `cache_none = true` to force caching None as well.
    // `is_smart_result`: cache only Ok(T), skip Err - always true for Result returns here;
    // opt out with `cache_err = true` to force caching Err values (in-memory default only).
    let is_smart_option = is_option_return && !args.cache_none;
    let is_smart_result = is_result_return && !args.cache_err;

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
    if args.result_fallback && !is_result_return {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a `Result<T, E>` return type",
        )
        .to_compile_error()
        .into();
    }
    if args.result_fallback && args.cache_err {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` and `cache_err` are mutually exclusive",
        )
        .to_compile_error()
        .into();
    }
    if args.result_fallback && args.with_cached_flag {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` and `with_cached_flag` are mutually exclusive: \
             `result_fallback` stores the inner `Ok(T)` value directly, but \
             `with_cached_flag` wraps the `Ok` value in `Return<T>` - the generated \
             code cannot simultaneously store `T` and expose `Return<T>` through \
             the cached function. Use `with_cached_flag = true` alone (without \
             `result_fallback`) or `result_fallback = true` alone.",
        )
        .to_compile_error()
        .into();
    }

    // `is_smart_option` on non-default paths is unsupported: stripping Option<T> requires
    // storing T, but redis/disk/custom stores are configured by the user to store the full
    // return type. The same check catches `cache_none = false` (default) on these paths.
    if is_smart_option {
        if args.redis {
            return syn::Error::new(
                fn_ident.span(),
                "`Option<T>` return types that skip `None` are only supported for the default \
                 in-memory sharded stores, not `redis = true`. \
                 Use `Result<T, E>` as the return type, or remove `redis = true` to use the \
                 default in-memory sharded path.",
            )
            .to_compile_error()
            .into();
        }
        if args.disk {
            return syn::Error::new(
                fn_ident.span(),
                "`Option<T>` return types that skip `None` are only supported for the default \
                 in-memory sharded stores, not `disk = true`. \
                 Use `Result<T, E>` as the return type, or remove `disk = true` to use the \
                 default in-memory sharded path.",
            )
            .to_compile_error()
            .into();
        }
        if args.ty.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`Option<T>` return types that skip `None` are only supported for the default \
                 in-memory sharded stores, not a custom `ty`. \
                 Use `Result<T, E>` as the return type, or remove `ty` to use the \
                 default in-memory sharded path.",
            )
            .to_compile_error()
            .into();
        }
        if args.create.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`Option<T>` return types that skip `None` are only supported for the default \
                 in-memory sharded stores, not a custom `create`. \
                 Use `Result<T, E>` as the return type, or remove `create` to use the \
                 default in-memory sharded path.",
            )
            .to_compile_error()
            .into();
        }
    }

    // if `with_cached_flag = true`, then enforce that the return type
    // is something wrapped in `Return`. Either `Return<T>` or the
    // fully qualified `cached::Return<T>`
    if check_with_cache_flag(args.with_cached_flag, &output) {
        return syn::Error::new(
            output_span,
            format!(
                "\nWhen specifying `with_cached_flag = true`, \
                    the return type must be wrapped in `cached::Return<T>`. \n\
                    The following return types are supported: \n\
                    |    `cached::Return<T>`\n\
                    |    `Result<cached::Return<T>, E>`\n\
                    |    `Option<cached::Return<T>>`\n\
                    Found type: {t}.",
                t = output_type_display
            ),
        )
        .to_compile_error()
        .into();
    }

    // `Option<Return<T>>` without smart-option mode would fall into the plain-Return<T>
    // branch and generate `result.value.clone()` on an `Option<Return<T>>` - a confusing
    // compile error with a bad span. Catch it here with a clear diagnostic.
    if args.with_cached_flag && !is_smart_option && is_option_return {
        return syn::Error::new(
            output_span,
            "`with_cached_flag = true` and `cache_none = true` are structurally incompatible \
             on `Option<T>` returns: `with_cached_flag` unwraps `Return<T>` and stores `T`, \
             while `cache_none = true` stores `Option<T>` as the cached value - the same \
             store cannot satisfy both. Remove one: use `with_cached_flag = true` alone to \
             receive a `Return<T>` that signals cache hits, or use `cache_none = true` alone \
             (without `with_cached_flag`) to cache `None` values.",
        )
        .to_compile_error()
        .into();
    }

    // Find the type of the value to store in the cache.
    // For Result<T, E> (is_smart_result): store the Ok type T.
    // For Option<T> (is_smart_option): store the inner T (None is not cached).
    // For with_cached_flag: unwrap Return<T> one further level to store T.
    // For plain return types (cache_err/cache_none opt-ins): store the return type as-is.
    let unable = format!(
        "#[concurrent_cached] unable to determine cache value type, found {output_type_display:?}"
    );
    let cache_value_ty = if is_smart_result {
        let ReturnType::Type(_, ty) = output.clone() else {
            unreachable!("is_smart_result=true implies ReturnType::Type")
        };

        // The `Ok` type of the function's `Result<..., E>`.
        let ok_ty = match first_type_arg(
            &ty,
            output_span,
            "function return type too complex, #[concurrent_cached] functions must return `Result`s",
            "#[concurrent_cached] functions must return `Result`s",
        ) {
            Ok(arg) => arg,
            Err(error) => return error.to_compile_error().into(),
        };

        if args.with_cached_flag {
            // Descend one more level into `cached::Return<T>` to recover `T`.
            // `check_with_cache_flag` above already verified the `Ok` type is
            // structurally `Return<...>`; gating on `with_cached_flag` (rather
            // than a bare-name token scan) avoids misclassifying an unrelated
            // type merely named `Return` (e.g. `Result<i32, Return>`).
            let GenericArgument::Type(return_ty) = ok_ty else {
                return syn::Error::new(output_span, &unable)
                    .to_compile_error()
                    .into();
            };
            match first_type_arg(return_ty, output_span, &unable, &unable) {
                Ok(inner) => quote! { #inner },
                Err(error) => return error.to_compile_error().into(),
            }
        } else {
            quote! { #ok_ty }
        }
    } else if is_smart_option {
        // Option<T> or Option<Return<T>>: strip the outer Option<_>.
        let ReturnType::Type(_, ty) = output.clone() else {
            unreachable!("is_smart_option=true implies ReturnType::Type")
        };
        let inner_ty = match first_type_arg(&ty, output_span, &unable, &unable) {
            Ok(arg) => arg,
            Err(error) => return error.to_compile_error().into(),
        };
        if args.with_cached_flag {
            // Option<Return<T>>: peel Return<T> one more level.
            let GenericArgument::Type(return_ty) = inner_ty else {
                return syn::Error::new(output_span, &unable)
                    .to_compile_error()
                    .into();
            };
            match first_type_arg(return_ty, output_span, &unable, &unable) {
                Ok(inner) => quote! { #inner },
                Err(error) => return error.to_compile_error().into(),
            }
        } else {
            quote! { #inner_ty }
        }
    } else {
        // Plain return type (cache_err = true, cache_none = true, or non-Result/Option type).
        // `with_cached_flag` stores the inner `T` from `Return<T>`.
        // Validation that this combination is only allowed on the infallible
        // in-memory sharded path happens after store selection below.
        match &output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, ty) => {
                if args.with_cached_flag {
                    match first_type_arg(ty, output_span, &unable, &unable) {
                        Ok(inner) => quote! { #inner },
                        Err(error) => return error.to_compile_error().into(),
                    }
                } else {
                    quote! { #ty }
                }
            }
        }
    };

    let with_cached_flag_result = args.with_cached_flag && is_smart_result;
    let with_cached_flag_option = args.with_cached_flag && is_smart_option;

    // make the cache identifier
    let cache_ident = match args.name {
        Some(ref name) => {
            if syn::parse_str::<syn::Ident>(name).is_err() {
                return syn::Error::new(fn_ident.span(), "`name` must be a valid Rust identifier")
                    .to_compile_error()
                    .into();
            }
            // G2: `__cached` prefix is reserved for macro-generated bindings.
            // Strip any leading `r#` before checking the reserved prefix so that
            // raw identifiers like `r#__cachedfoo` are also rejected.
            let bare = name.strip_prefix("r#").unwrap_or(name);
            if bare.starts_with("__cached") {
                return syn::Error::new(
                    fn_ident.span(),
                    "cache names beginning with `__cached` are reserved for macro-generated \
                     bindings and cannot be used as a `name` value",
                )
                .to_compile_error()
                .into();
            }
            match name.strip_prefix("r#") {
                Some(stripped) => Ident::new_raw(stripped, fn_ident.span()),
                None => Ident::new(name, fn_ident.span()),
            }
        }
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };
    let cache_name = cache_ident.to_string();

    let (cache_key_ty, key_convert_block) =
        match make_cache_key_type(&args.key, &args.convert, &args.ty, input_tys, &input_names) {
            Ok(key) => key,
            Err(error) => return error.to_compile_error().into(),
        };

    // `has_ttl` / `ttl_duration` were resolved above (from `ttl` expr, `ttl_secs`,
    // or `ttl_millis`). `has_ttl` drives store selection and the `result_fallback`
    // TTL-presence check (#149). `ttl_duration` is passed into the store-selector
    // helpers so every backend (redis/disk/sharded) honors whichever unit was used.

    // Track whether the cache uses Infallible errors (the in-memory sharded default).
    // When true, `map_error` is a compile error and cache ops use `.expect(...)`.
    let mut infallible_default = false;

    // make the cache type and create statement
    let (cache_ty, cache_create) = match (&args.redis, &args.disk) {
        (true, false) => {
            if args.shards.is_some() {
                return syn::Error::new(
                    fn_ident.span(),
                    "`shards` only applies to the default in-memory store, not `redis = true`",
                )
                .to_compile_error()
                .into();
            }
            if args.max_size.is_some() {
                return syn::Error::new(
                    fn_ident.span(),
                    "`max_size` only applies to the default in-memory store, not `redis = true`",
                )
                .to_compile_error()
                .into();
            }
            match get_redis_cache_type_and_create(
                &args,
                &krate,
                ttl_duration.as_ref(),
                &cache_ident,
                &cache_key_ty,
                &cache_value_ty,
                asyncness.is_some(),
            ) {
                Ok(v) => v,
                Err(e) => return e.to_compile_error().into(),
            }
        }
        (false, true) => {
            if args.shards.is_some() {
                return syn::Error::new(
                    fn_ident.span(),
                    "`shards` only applies to the default in-memory store, not `disk = true`",
                )
                .to_compile_error()
                .into();
            }
            if args.max_size.is_some() {
                return syn::Error::new(
                    fn_ident.span(),
                    "`max_size` only applies to the default in-memory store, not `disk = true`",
                )
                .to_compile_error()
                .into();
            }
            match get_disk_cache_type_and_create(
                &args,
                &krate,
                ttl_duration.as_ref(),
                &cache_name,
                &cache_key_ty,
                &cache_value_ty,
                &fn_ident,
            ) {
                Ok(v) => v,
                Err(e) => return e.to_compile_error().into(),
            }
        }
        (true, true) => {
            return syn::Error::new(
                fn_ident.span(),
                "`redis = true` and `disk = true` are mutually exclusive",
            )
            .to_compile_error()
            .into();
        }
        _ => {
            // Default cascade: when no `redis`/`disk` and no custom `ty`/`create`, fall
            // back to one of the sharded in-memory stores. With `ty` or `create`, the
            // user supplies everything and we delegate to the custom path.
            if args.ty.is_some() || args.create.is_some() {
                match get_custom_cache_type_and_create(&args, &fn_ident) {
                    Ok(v) => v,
                    Err(e) => return e.to_compile_error().into(),
                }
            } else {
                match get_sharded_cache_type_and_create(
                    &args,
                    &krate,
                    ttl_duration.as_ref(),
                    &cache_key_ty,
                    &cache_value_ty,
                    &fn_ident,
                ) {
                    Ok(v) => {
                        infallible_default = true;
                        v
                    }
                    Err(e) => return e.to_compile_error().into(),
                }
            }
        }
    };

    // cache_none / cache_err are only valid for the in-memory sharded default path; give
    // targeted errors before the generic non-Result check below so the message names the
    // offending attribute rather than the return type.
    if args.cache_none && !infallible_default {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_none = true` is only supported for the default in-memory sharded stores",
        )
        .to_compile_error()
        .into();
    }

    // Non-Result return types (plain, or Option<T> after cache_none is handled above) are only
    // valid for the in-memory sharded default path. Result<T, E> return types work with all
    // store backends (redis/disk/custom).
    if !is_result_return && !infallible_default {
        let kind = if is_option_return {
            "Option<T>"
        } else {
            "plain"
        };
        return syn::Error::new(
            output_span,
            format!(
                "#[concurrent_cached] {kind} return types are only supported for the default \
                 in-memory sharded stores. Use `Result<T, E>` when specifying `redis`, `disk`, \
                 or a custom `ty`/`create`."
            ),
        )
        .to_compile_error()
        .into();
    }

    if args.cache_err && !infallible_default {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_err = true` is only supported for the default in-memory sharded stores",
        )
        .to_compile_error()
        .into();
    }

    if args.result_fallback && !infallible_default {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` is only supported for the default in-memory sharded stores",
        )
        .to_compile_error()
        .into();
    }

    if args.result_fallback && !has_ttl {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) to be set (e.g. `ttl_secs = 60`). It serves the last \
             cached `Ok` value when a refresh returns `Err`, but a refresh only happens after an \
             entry expires. Without a TTL entries never expire, so the function body is never \
             re-run for a cached key and the fallback can never fire - making the option a no-op. \
             Set a TTL so cached entries expire and `result_fallback` has something to fall back to.",
        )
        .to_compile_error()
        .into();
    }

    // Resolve the cache-error handling strategy.  For the default sharded
    // in-memory stores the error type is `Infallible`, so cache operations can
    // never fail and `.expect(...)` is always correct.  `map_error` is rejected on
    // this path - there are no errors to map.
    //
    // For the fallible redis / disk / custom paths, if the user supplies
    // `map_error = |e| ...`, we emit `.map_err(closure)?`. If `map_error` is
    // absent on a fallible path, we emit `.map_err(::std::convert::Into::into)?`,
    // which works when the function's error type implements `From<StoreError>`.
    let map_error_closure: Option<syn::Expr> = match (&args.map_error, infallible_default) {
        (Some(_), true) => {
            return syn::Error::new(
                fn_ident.span(),
                "`map_error` is not applicable to the default in-memory sharded stores - \
                 their error type is `Infallible` and cache operations cannot fail. \
                 Remove `map_error`, or add `redis = true`, `disk = true`, or a custom \
                 `ty`/`create` to use a store with a fallible error type.",
            )
            .to_compile_error()
            .into();
        }
        (Some(expr), false) => {
            // Verify the expression is a closure; other expression types are not
            // valid for `map_error`.
            if !matches!(expr, syn::Expr::Closure(_)) {
                return syn::Error::new_spanned(
                    expr,
                    "`map_error` must be a closure, e.g. `map_error = |e| MyErr(e)`",
                )
                .to_compile_error()
                .into();
            }
            Some(expr.clone())
        }
        (None, true) => None,  // infallible: use .expect(...) in generated code
        (None, false) => None, // fallible but no map_error: use Into::into
    };

    // Emit either `.map_err(closure)?`, bare `?`, or `.expect(...)`
    // for fallible stores or infallible stores.
    let (cache_get_unwrap, cache_set_unwrap): (proc_macro2::TokenStream, proc_macro2::TokenStream) =
        if infallible_default {
            // The store's error type is `Infallible`; these `.expect()`s are unreachable.
            let msg = "cache operation on the default in-memory sharded store is infallible";
            (quote! { .expect(#msg) }, quote! { .expect(#msg) })
        } else if let Some(me) = &map_error_closure {
            (quote! { .map_err(#me)? }, quote! { .map_err(#me)? })
        } else {
            // No explicit map_error on a fallible path: use `?` directly so that the
            // standard `From` trait machinery handles the conversion. `?` on a
            // `Result<_, StoreError>` in a function returning `Result<_, E>` calls
            // `E::from(e)`, which requires only `E: From<StoreError>`. This avoids the
            // type-inference ambiguity that `.map_err(Into::into)` can produce when the
            // target error type has multiple `From` implementations.
            (quote! { ? }, quote! { ? })
        };

    // Resolve companion fn visibility (#9).
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
    // For `await`-ed variants we need identical logic.
    let cache_get_unwrap_async = cache_get_unwrap.clone();
    let cache_set_unwrap_async = cache_set_unwrap.clone();

    // Emit a cache-set call. `value_ref` is an expression that already evaluates to
    // a `&V`. The set goes through the `__set_dispatch` autoref shim: when the
    // concrete store implements `SerializeCached`/`SerializeCachedAsync` (redis, disk,
    // or any custom `ty`/`create` store that does) the borrowed setter is used and the
    // value is serialized from the reference with no pre-set clone (#196); otherwise
    // the shim clones the value and calls the owned `cache_set`. The key is moved in
    // either way (no key clone), matching the previous owned path. The `use ... as _;`
    // brings the fallback trait into scope so method resolution can reach it; the
    // inherent (serialize) method is always preferred when it applies.
    let set_call = |value_ref: proc_macro2::TokenStream| {
        if asyncness.is_some() {
            quote! {
                {
                    use #krate::__set_dispatch_async::SetDispatchAsyncFallback as _;
                    #krate::__set_dispatch_async::SetDispatchAsync::new(__cached_cache)
                        .cache_set_dispatch(__cached_key, #value_ref).await #cache_set_unwrap_async;
                }
            }
        } else {
            quote! {
                {
                    use #krate::__set_dispatch::SetDispatchFallback as _;
                    #krate::__set_dispatch::SetDispatch::new(__cached_cache)
                        .cache_set_dispatch(__cached_key, #value_ref) #cache_set_unwrap;
                }
            }
        }
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = if with_cached_flag_result {
        // Result<Return<T>, E>: cache the inner T from Ok(Return<T>).
        let set = set_call(quote! { &**__cached_inner });
        (
            quote! {
                if let Ok(__cached_inner) = &__cached_result {
                    #set
                }
            },
            quote! { let mut __cached_r = #krate::Return::new(__cached_result); __cached_r.set_was_cached(true); return Ok(__cached_r) },
        )
    } else if with_cached_flag_option {
        // Option<Return<T>>: cache the inner T from Some(Return<T>), skip None.
        let set = set_call(quote! { &**__cached_inner });
        (
            quote! {
                if let Some(__cached_inner) = &__cached_result {
                    #set
                }
            },
            quote! { let mut __cached_r = #krate::Return::new(__cached_result); __cached_r.set_was_cached(true); return Some(__cached_r) },
        )
    } else if args.with_cached_flag {
        // Plain Return<T>: cache the inner T directly.
        let set = set_call(quote! { &*__cached_result });
        (
            set,
            quote! { let mut __cached_r = #krate::Return::new(__cached_result); __cached_r.set_was_cached(true); return __cached_r },
        )
    } else if is_smart_result {
        // Result<T, E> return type: cache only Ok(T), skip Err
        let set = set_call(quote! { __cached_inner });
        (
            quote! {
                if let Ok(__cached_inner) = &__cached_result {
                    #set
                }
            },
            quote! { return Ok(__cached_result) },
        )
    } else if is_smart_option {
        // Option<T>: cache Some(T), skip None. infallible_default guaranteed.
        let set = set_call(quote! { __cached_inner });
        (
            quote! {
                if let Some(__cached_inner) = &__cached_result {
                    #set
                }
            },
            quote! { return Some(__cached_result) },
        )
    } else {
        // Plain return type - infallible_default is guaranteed true here.
        // No Ok/Err wrapping: the result is the value directly.
        let set = set_call(quote! { &__cached_result });
        (set, quote! { return __cached_result })
    };

    // Clone the full original signature and rename it to `__cached_inner`. Quoting
    // the whole `syn::Signature` preserves the `where` clause (and lifetimes,
    // const generics, etc.) - `#generics` alone drops the where clause.
    // Unique per-function name so multiple `in_impl` methods on the same impl
    // block do not collide on a shared `__cached_inner` sibling method.
    let inner_fn_ident = Ident::new(&format!("{}_no_cache", &fn_ident), fn_ident.span());
    let mut inner_sig = signature.clone();
    inner_sig.ident = inner_fn_ident.clone();

    // For `in_impl` methods the body may reference `self`, so `__cached_inner`
    // must be a sibling impl method (a nested fn cannot capture `self`) invoked
    // as `self.__cached_inner(...)`. For free functions it stays a nested fn
    // defined inline in the body (#16/#140).
    let self_prefix = if has_receiver {
        quote! { self. }
    } else {
        quote! {}
    };
    // The `in_impl` origin sibling is a public impl method; hide it from consumers'
    // rustdoc with `#[doc(hidden)]` (it stays callable as an escape hatch).
    let (inner_sibling_def, inner_nested_def) = if args.in_impl {
        (
            quote! { #[doc(hidden)] #companions_visibility #inner_sig #body },
            quote! {},
        )
    } else {
        (quote! {}, quote! { #inner_sig #body })
    };

    // `force_refresh`: opt-in boolean expression block over the fn args, in curly
    // braces like `convert` (e.g. `force_refresh = "{ id == 0 }"`); when `true`,
    // skip the cached-hit early return so the body re-runs and re-caches.
    // Orthogonal to `refresh` (TTL renewal on hit) (#146).
    // Parse the `force_refresh` predicate once; both the cached-hit guard and the
    // `result_fallback` bypass token below are built from this single parsed block.
    let force_refresh_block = match parse_force_refresh_block(&args.force_refresh, fn_ident.span())
    {
        Ok(block) => block,
        Err(error) => return error.to_compile_error().into(),
    };

    let force_refresh_guard = match &force_refresh_block {
        Some(block) => quote! { if !(#block) },
        None => quote! { if true },
    };

    // `force_refresh_bypass`: the force-refresh predicate as a plain boolean expression
    // (`(#block)`), or constant `false` when there is no `force_refresh`. Used by the
    // `result_fallback` path to decide, once, whether a present entry is being bypassed.
    // When bypassing we read the stale fallback value via the non-renewing
    // `cache_peek_with_expiry_status` so the bypassed entry sees no read side effects (#146);
    // when not bypassing we use the renewing `cache_get_with_expiry_status`, which is the
    // correct read for a genuine hit. With no `force_refresh` this is constant-false, so the
    // renewing-read + early-return path is always taken — equivalent to the prior behavior.
    let force_refresh_bypass = match &force_refresh_block {
        Some(block) => quote! { (#block) },
        None => quote! { false },
    };

    // The cache-set used on the `result_fallback` Ok path. `__cached_ok_val` is a
    // `&V` (bound via `if let Ok(__cached_ok_val) = &__cached_result`). Routing it
    // through `set_call` keeps every set site on one path and moves the key instead
    // of cloning it (the owned `cache_set` call cloned both key and value).
    //
    // Note: `result_fallback` is gated to the in-memory sharded stores, which do not
    // implement `SerializeCached`/`SerializeCachedAsync`, so today the shim always
    // resolves to its owned-fallback arm (clones the value, same as before). The
    // borrowed clone-eliding arm is therefore currently unreachable here; it would
    // engage automatically only if a serialize-backed store were ever admitted to
    // the `result_fallback` path (which also needs `ConcurrentCloneCached`).
    let fallback_set = set_call(quote! { __cached_ok_val });

    // `do_set_return_block`: runs `__cached_inner`, sets the cache, returns the result.
    // For `result_fallback`, the expiry-aware lookup (via `ConcurrentCloneCached`) is folded
    // into this block; no separate `_FALLBACK` static is needed.
    //
    // When `force_refresh` bypasses a present entry, the stale fallback value is captured via
    // the non-renewing `cache_peek_with_expiry_status` so the bypassed entry has no read side
    // effects (#146); a genuine (non-bypass) hit still uses the renewing
    // `cache_get_with_expiry_status` and takes the early `#return_cache_block`.
    let do_set_return_block = if args.result_fallback && asyncness.is_some() {
        quote! {
            #inner_nested_def
            let __cached_cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            let __cached_old_val = if #force_refresh_bypass {
                // Bypassing this entry: peek for the stale fallback without side effects.
                let (__cached_stale, _) = #krate::ConcurrentCloneCached::cache_peek_with_expiry_status(__cached_cache, &__cached_key);
                __cached_stale
            } else {
                let (__cached_val, __cached_expired) = #krate::ConcurrentCloneCached::cache_get_with_expiry_status(__cached_cache, &__cached_key);
                match (__cached_val, __cached_expired) {
                    (Some(__cached_result), false) => { #return_cache_block }
                    (__cached_stale, _) => __cached_stale,
                }
            };
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*).await;
            let __cached_result = match (__cached_result.is_err(), __cached_old_val) {
                (true, Some(__cached_old_val)) => Ok(__cached_old_val),
                _ => __cached_result,
            };
            if let Ok(__cached_ok_val) = &__cached_result {
                #fallback_set
            }
            __cached_result
        }
    } else if args.result_fallback {
        quote! {
            #inner_nested_def
            let __cached_cache = &*#cache_ident;
            let __cached_old_val = if #force_refresh_bypass {
                // Bypassing this entry: peek for the stale fallback without side effects.
                let (__cached_stale, _) = #krate::ConcurrentCloneCached::cache_peek_with_expiry_status(__cached_cache, &__cached_key);
                __cached_stale
            } else {
                let (__cached_val, __cached_expired) = #krate::ConcurrentCloneCached::cache_get_with_expiry_status(__cached_cache, &__cached_key);
                match (__cached_val, __cached_expired) {
                    (Some(__cached_result), false) => { #return_cache_block }
                    (__cached_stale, _) => __cached_stale,
                }
            };
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*);
            let __cached_result = match (__cached_result.is_err(), __cached_old_val) {
                (true, Some(__cached_old_val)) => Ok(__cached_old_val),
                _ => __cached_result,
            };
            if let Ok(__cached_ok_val) = &__cached_result {
                #fallback_set
            }
            __cached_result
        }
    } else if asyncness.is_some() {
        quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*).await;
            let __cached_cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            #set_cache_block
            __cached_result
        }
    } else {
        quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*);
            let __cached_cache = &*#cache_ident;
            #set_cache_block
            __cached_result
        }
    };

    let signature_no_muts = get_mut_signature(signature);

    // create a signature for the cache-priming function
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

    // `prime_do_set_return_block`: used by the priming function. For `result_fallback`,
    // prime unconditionally reruns the function and stores the result - no old_val fallback,
    // no early-return on fresh hit. For all other paths, prime reuses `do_set_return_block`
    // which already implements "run inner and set cache".
    let prime_do_set_return_block = if args.result_fallback && asyncness.is_some() {
        quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*).await;
            let __cached_cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            if let Ok(__cached_ok_val) = &__cached_result {
                #fallback_set
            }
            __cached_result
        }
    } else if args.result_fallback {
        quote! {
            #inner_nested_def
            let __cached_result = #self_prefix #inner_fn_ident(#(#input_names),*);
            let __cached_cache = &*#cache_ident;
            if let Ok(__cached_ok_val) = &__cached_result {
                #fallback_set
            }
            __cached_result
        }
    } else {
        do_set_return_block.clone()
    };

    // `initial_cache_lookup`: the early-return guard block emitted at the start of the cached
    // function body. For `result_fallback`, the lookup is folded into `do_set_return_block`
    // (via `ConcurrentCloneCached`), so we emit nothing here for that path.
    // The `#force_refresh_guard` wraps the whole lookup (not just the early
    // return) so the `cache_get` call is skipped when force-refreshing. On a
    // `refresh_on_hit` TTL store, `cache_get` renews the entry's TTL as a side
    // effect, which must not happen for a bypassed entry (#146).
    let initial_cache_lookup_async = if args.result_fallback {
        quote! {}
    } else {
        quote! {
            {
                // check if the result is cached
                let __cached_cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
                #force_refresh_guard {
                    if let Some(__cached_result) = #krate::ConcurrentCachedAsync::async_cache_get(__cached_cache, &__cached_key).await #cache_get_unwrap_async {
                        #return_cache_block
                    }
                }
            }
        }
    };
    let initial_cache_lookup_sync = if args.result_fallback {
        quote! {}
    } else {
        quote! {
            {
                // check if the result is cached
                let __cached_cache = &*#cache_ident;
                #force_refresh_guard {
                    if let Some(__cached_result) = #krate::ConcurrentCached::cache_get(__cached_cache, &__cached_key) #cache_get_unwrap {
                        #return_cache_block
                    }
                }
            }
        }
    };

    // The cache static cannot sit at impl scope when `in_impl`; emit it inside
    // each generated fn body instead (also fixes same-named-method collisions).
    // Build the static with a caller-supplied leading visibility token: the
    // module-scope static keeps the method's `#visibility`, but the `in_impl`
    // function-local static is emitted bare (no visibility): a visibility on a
    // function-local item is meaningless and trips `unreachable_pub` (#7).
    let make_static = |vis: &proc_macro2::TokenStream| {
        if asyncness.is_some() {
            quote! {
                #vis static #cache_ident: #krate::async_sync::OnceCell<#cache_ty> = #krate::async_sync::OnceCell::new();
            }
        } else {
            quote! {
                #vis static #cache_ident: ::std::sync::LazyLock<#cache_ty> = ::std::sync::LazyLock::new(|| #cache_create);
            }
        }
    };
    let (module_static, body_static) = if args.in_impl {
        // No `#[doc]`: a function-local static is not part of the public API and
        // rustdoc ignores doc attributes on it, so the doc string would be dead.
        (quote! {}, make_static(&quote! {}))
    } else {
        let static_decl = make_static(&quote! { #visibility });
        (quote! { #[doc = #cache_ident_doc] #static_decl }, quote! {})
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
                let __cached_key = #key_convert_block;
                #prime_do_set_return_block
            }
        }
    };

    // put it all together
    let expanded = if asyncness.is_some() {
        quote! {
            // Cached static (module scope unless `in_impl`)
            #module_static
            // Inner origin fn as a sibling impl method (only when `in_impl`)
            #inner_sibling_def
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                #body_static
                let __cached_key = #key_convert_block;
                #initial_cache_lookup_async
                #do_set_return_block
            }
            // Prime cached function (omitted for `in_impl` methods)
            #prime_fn
        }
    } else {
        quote! {
            // Cached static (module scope unless `in_impl`)
            #module_static
            // Inner origin fn as a sibling impl method (only when `in_impl`)
            #inner_sibling_def
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                #body_static
                let __cached_key = #key_convert_block;
                #initial_cache_lookup_sync
                #do_set_return_block
            }
            // Prime cached function (omitted for `in_impl` methods)
            #prime_fn
        }
    };

    expanded.into()
}

fn get_redis_cache_type_and_create(
    args: &ConcurrentCachedArgs,
    krate: &proc_macro2::TokenStream,
    ttl_duration: Option<&proc_macro2::TokenStream>,
    cache_ident: &Ident,
    cache_key_ty: &proc_macro2::TokenStream,
    cache_value_ty: &proc_macro2::TokenStream,
    is_async: bool,
) -> Result<(proc_macro2::TokenStream, proc_macro2::TokenStream), syn::Error> {
    // `durable` / `disk_dir` configure the redb disk store only. Reject them on the
    // redis path rather than silently ignoring them (the in-memory and `create`
    // paths reject them too).
    let mut disk_only = Vec::new();
    if args.disk_dir.is_some() {
        disk_only.push("disk_dir");
    }
    if args.durable.is_some() {
        disk_only.push("durable");
    }
    if !disk_only.is_empty() {
        let list = disk_only
            .iter()
            .map(|a| format!("`{a}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(syn::Error::new(
            cache_ident.span(),
            format!(
                "{list} can only be used with `disk = true` (the redb store), not the redis path"
            ),
        ));
    }

    // A custom `ty` on the redis path is only honored when the user also supplies a
    // matching `create` block. Without `create` the macro would build the DEFAULT store
    // (`RedisCache`/`AsyncRedisCache`) via its default builder while declaring the cache
    // as the custom `ty`, silently mispairing the two. Reject that up front instead.
    if args.ty.is_some() && args.create.is_none() {
        return Err(syn::Error::new(
            cache_ident.span(),
            "a custom `ty` on the redis path requires a matching `create` block: without \
             `create` the macro would construct the default `RedisCache`/`AsyncRedisCache` \
             store, which would not match `ty`",
        ));
    }

    let cache_ty = match &args.ty {
        Some(ty) => {
            let ty = parse_str::<Type>(ty).map_err(|e| {
                syn::Error::new(
                    cache_ident.span(),
                    format!("unable to parse cache type: {e}"),
                )
            })?;
            quote! { #ty }
        }
        None => {
            if is_async {
                quote! { #krate::AsyncRedisCache<#cache_key_ty, #cache_value_ty> }
            } else {
                quote! { #krate::RedisCache<#cache_key_ty, #cache_value_ty> }
            }
        }
    };
    let cache_create = match &args.create {
        Some(create_expr) => {
            check_create_conflicts(args, cache_ident.span())?;
            expr_value_tokens(create_expr)
        }
        None => {
            if let Some(ttl_dur) = ttl_duration {
                let cache_prefix_block: proc_macro2::TokenStream = if let Some(cp_expr) =
                    &args.cache_prefix_block
                {
                    // User supplied a `cache_prefix_block` expression.
                    expr_value_tokens(cp_expr)
                } else {
                    // Runtime key-prefix string: NOT a path into the `cached`
                    // crate, so it is intentionally left as the literal
                    // `cached::macros::...` namespace (do not rewrite to `#krate`).
                    let prefix_str = format!("cached::macros::concurrent_cached::{}", cache_ident);
                    quote! { { #prefix_str } }
                };
                let refresh = args.refresh;
                if is_async {
                    quote! { #krate::AsyncRedisCache::builder().prefix(#cache_prefix_block).ttl(#ttl_dur).refresh_on_hit(#refresh).build().await.unwrap_or_else(|e| panic!("error constructing AsyncRedisCache in #[concurrent_cached] macro: {e}")) }
                } else {
                    quote! {
                        #krate::RedisCache::builder().prefix(#cache_prefix_block).ttl(#ttl_dur).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("error constructing RedisCache in #[concurrent_cached] macro: {e}"))
                    }
                }
            } else if is_async {
                return Err(syn::Error::new(
                    cache_ident.span(),
                    "AsyncRedisCache requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) when `create` block is not specified",
                ));
            } else {
                return Err(syn::Error::new(
                    cache_ident.span(),
                    "RedisCache requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) when `create` block is not specified",
                ));
            }
        }
    };
    Ok((cache_ty, cache_create))
}

fn get_disk_cache_type_and_create(
    args: &ConcurrentCachedArgs,
    krate: &proc_macro2::TokenStream,
    ttl_duration: Option<&proc_macro2::TokenStream>,
    cache_name: &str,
    cache_key_ty: &proc_macro2::TokenStream,
    cache_value_ty: &proc_macro2::TokenStream,
    fn_ident: &Ident,
) -> Result<(proc_macro2::TokenStream, proc_macro2::TokenStream), syn::Error> {
    // A custom `ty` on the disk path is only honored when the user also supplies a
    // matching `create` block. Without `create` the macro would build the DEFAULT store
    // (`RedbCache`) via its default builder while declaring the cache as the custom `ty`,
    // silently mispairing the two. Reject that up front instead.
    if args.ty.is_some() && args.create.is_none() {
        return Err(syn::Error::new(
            fn_ident.span(),
            "a custom `ty` on the disk path requires a matching `create` block: without \
             `create` the macro would construct the default `RedbCache` store, which would \
             not match `ty`",
        ));
    }

    let cache_ty = match &args.ty {
        Some(ty) => {
            let ty = parse_str::<Type>(ty).map_err(|e| {
                syn::Error::new(fn_ident.span(), format!("unable to parse cache type: {e}"))
            })?;
            quote! { #ty }
        }
        None => {
            quote! { #krate::RedbCache<#cache_key_ty, #cache_value_ty> }
        }
    };
    let cache_create = match &args.create {
        Some(create_expr) => {
            check_create_conflicts(args, fn_ident.span())?;
            expr_value_tokens(create_expr)
        }
        None => {
            let create = quote! {
                #krate::RedbCache::builder().name(#cache_name)
            };
            let create = match ttl_duration {
                None => create,
                Some(ttl_dur) => {
                    quote! {
                        (#create).ttl(#ttl_dur)
                    }
                }
            };
            let refresh = args.refresh;
            let create = quote! { (#create).refresh_on_hit(#refresh) };
            let create = match args.durable {
                None => create,
                Some(durable) => {
                    quote! {
                        (#create).durable(#durable)
                    }
                }
            };
            let create = match &args.disk_dir {
                None => create,
                Some(disk_dir) => {
                    quote! { (#create).disk_directory(#disk_dir) }
                }
            };
            quote! { (#create).build().unwrap_or_else(|e| panic!("error constructing RedbCache in #[concurrent_cached] macro: {e}")) }
        }
    };
    Ok((cache_ty, cache_create))
}

/// Default in-memory sharded store selector.
///
/// Selects one of the four `Sharded*Cache` variants based on the `max_size` / `ttl`
/// attributes supplied to the macro:
///
/// | max_size | ttl | expires | store |
/// |----------|-----|---------|-------|
/// |  no  |  no |   no    | `ShardedUnboundCache` |
/// | yes  |  no |   no    | `ShardedLruCache` |
/// |  no  | yes |   no    | `ShardedTtlCache`         (requires `time_stores` feature on `cached`) |
/// | yes  | yes |   no    | `ShardedLruTtlCache`      (requires `time_stores` feature on `cached`) |
/// |  no  |  -  |   yes   | `ShardedExpiringCache`    (per-value expiry; `ttl` is rejected with `expires`) |
/// | yes  |  -  |   yes   | `ShardedExpiringLruCache` (per-value expiry; `ttl` is rejected with `expires`) |
///
/// `shards = N` is honored on every variant and routes through the `_and_shards`
/// shortcut constructor.
fn get_sharded_cache_type_and_create(
    args: &ConcurrentCachedArgs,
    krate: &proc_macro2::TokenStream,
    ttl_duration: Option<&proc_macro2::TokenStream>,
    cache_key_ty: &proc_macro2::TokenStream,
    cache_value_ty: &proc_macro2::TokenStream,
    fn_ident: &Ident,
) -> Result<(proc_macro2::TokenStream, proc_macro2::TokenStream), syn::Error> {
    if matches!(args.shards, Some(0)) {
        return Err(syn::Error::new(fn_ident.span(), "`shards` must be >= 1"));
    }
    if matches!(args.max_size, Some(0)) {
        return Err(syn::Error::new(fn_ident.span(), "`max_size` must be >= 1"));
    }
    // No `ttl`/`ttl_secs`/`ttl_millis` zero check here: a zero `ttl_secs`/`ttl_millis`
    // is rejected at the top level of the macro (shared by every store path) before
    // this helper runs, and `ttl` (a Duration expression) has no compile-time value.
    if args.refresh && ttl_duration.is_none() {
        return Err(syn::Error::new(
            fn_ident.span(),
            "`refresh` requires a TTL (`ttl`/`ttl_secs`/`ttl_millis`) to be set on the default in-memory sharded path",
        ));
    }

    // Reject attributes that don't apply to the in-memory default path.
    let mut conflicting = Vec::new();
    if args.cache_prefix_block.is_some() {
        conflicting.push("cache_prefix_block");
    }
    if args.disk_dir.is_some() {
        conflicting.push("disk_dir");
    }
    if args.durable.is_some() {
        conflicting.push("durable");
    }
    if !conflicting.is_empty() {
        let list = conflicting
            .iter()
            .map(|a| format!("`{a}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(syn::Error::new(
            fn_ident.span(),
            format!(
                "{list} only apply to the redis/disk paths; for the default in-memory \
                 sharded store remove these attributes or provide a custom `create` block"
            ),
        ));
    }

    let (cache_ty, cache_create) = if args.expires {
        match args.max_size {
            Some(size) => {
                let ty = quote! { #krate::ShardedExpiringLruCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { #krate::ShardedExpiringLruCache::builder().max_size(#size).shards(#n).build().unwrap_or_else(|e| panic!("ShardedExpiringLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { #krate::ShardedExpiringLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ShardedExpiringLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            None => {
                let ty = quote! { #krate::ShardedExpiringCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { #krate::ShardedExpiringCache::builder().shards(#n).build().unwrap_or_else(|e| panic!("ShardedExpiringCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { #krate::ShardedExpiringCache::builder().build().unwrap_or_else(|e| panic!("ShardedExpiringCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
        }
    } else {
        match (args.max_size, ttl_duration) {
            (None, None) => {
                let ty = quote! { #krate::ShardedUnboundCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { #krate::ShardedUnboundCache::builder().shards(#n).build().unwrap_or_else(|e| panic!("ShardedUnboundCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { #krate::ShardedUnboundCache::builder().build().unwrap_or_else(|e| panic!("ShardedUnboundCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            (Some(size), None) => {
                let ty = quote! { #krate::ShardedLruCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { #krate::ShardedLruCache::builder().max_size(#size).shards(#n).build().unwrap_or_else(|e| panic!("ShardedLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { #krate::ShardedLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ShardedLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            (None, Some(ttl_dur)) => {
                let ty = quote! { #krate::ShardedTtlCache<#cache_key_ty, #cache_value_ty> };
                let refresh = args.refresh;
                let create = match args.shards {
                    Some(n) => quote! {{
                        let __c = #krate::ShardedTtlCache::builder()
                            .ttl(#ttl_dur)
                            .shards(#n)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                    None => quote! {{
                        let __c = #krate::ShardedTtlCache::builder()
                            .ttl(#ttl_dur)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                };
                (ty, create)
            }
            (Some(size), Some(ttl_dur)) => {
                let ty = quote! { #krate::ShardedLruTtlCache<#cache_key_ty, #cache_value_ty> };
                let refresh = args.refresh;
                let create = match args.shards {
                    Some(n) => quote! {{
                        let __c = #krate::ShardedLruTtlCache::builder()
                            .max_size(#size)
                            .ttl(#ttl_dur)
                            .shards(#n)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedLruTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                    None => quote! {{
                        let __c = #krate::ShardedLruTtlCache::builder()
                            .max_size(#size)
                            .ttl(#ttl_dur)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedLruTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                };
                (ty, create)
            }
        }
    };

    Ok((cache_ty, cache_create))
}

fn get_custom_cache_type_and_create(
    args: &ConcurrentCachedArgs,
    fn_ident: &Ident,
) -> Result<(proc_macro2::TokenStream, proc_macro2::TokenStream), syn::Error> {
    let cache_ty = match &args.ty {
        Some(ty) => {
            let ty = parse_str::<Type>(ty).map_err(|e| {
                syn::Error::new(fn_ident.span(), format!("unable to parse cache type: {e}"))
            })?;
            quote! { #ty }
        }
        None => {
            return Err(syn::Error::new(
                fn_ident.span(),
                "`create` requires `ty` to also be set",
            ));
        }
    };
    let cache_create = match &args.create {
        Some(create_expr) => {
            check_create_conflicts(args, fn_ident.span())?;
            expr_value_tokens(create_expr)
        }
        None => {
            return Err(syn::Error::new(
                fn_ident.span(),
                "`ty` requires `create` to also be set",
            ));
        }
    };
    Ok((cache_ty, cache_create))
}
