use crate::helpers::*;
use darling::FromMeta;
use darling::ast::NestedMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Block, ExprClosure, GenericArgument, Ident, ItemFn, ReturnType, Type, parse_macro_input,
    parse_str,
};

#[derive(FromMeta)]
struct ConcurrentCachedArgs {
    #[darling(default)]
    map_error: Option<String>,
    #[darling(default)]
    disk: bool,
    #[darling(default)]
    disk_dir: Option<String>,
    #[darling(default)]
    redis: bool,
    #[darling(default)]
    cache_prefix_block: Option<String>,
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    ttl: Option<u64>,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    time_refresh: Option<bool>,
    /// Removed alias for `max_size`; kept only to emit a helpful rename error.
    #[darling(default)]
    size: Option<usize>,
    #[darling(default)]
    refresh: Option<bool>,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<String>,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    cache_err: bool,
    #[darling(default)]
    cache_none: bool,
    /// When `true`, an `Err` return serves the last cached `Ok` value for that key.
    /// Requires `ttl`. The stale value is read from the primary TTL cache slot via
    /// `ConcurrentCloneCached::cache_get_with_expiry_status` (no separate store is
    /// created) and re-cached with a fresh TTL window on `Err`.
    #[darling(default)]
    result_fallback: bool,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<String>,
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
}

/// When a `create` block is supplied the user fully constructs the store, so
/// every store-builder attribute the macro would otherwise apply is dropped.
/// Reject those attributes with a precise message instead of silently ignoring
/// them — otherwise `disk_dir` / `durable` (and `ttl` /
/// `refresh` / `cache_prefix_block`) look applied but are not.
fn check_create_conflicts(
    args: &ConcurrentCachedArgs,
    span: proc_macro2::Span,
) -> Result<(), syn::Error> {
    let mut conflicting = Vec::new();
    if args.ttl.is_some() {
        conflicting.push("ttl");
    }
    if args.refresh.is_some() {
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
            "cannot specify {list} when passing a `create` block — `create` fully \
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
                "`sync_writes` is not supported by #[concurrent_cached]; concurrent stores \
                 synchronize cache access internally but do not deduplicate first-call execution",
            ),
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

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness;

    if inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Receiver(_)))
    {
        return syn::Error::new(
            fn_ident.span(),
            "#[concurrent_cached] cannot be applied to methods that take `self`",
        )
        .to_compile_error()
        .into();
    }

    if args.time.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time` was renamed to `ttl` in cached 1.0; use `ttl = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.time_refresh.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time_refresh` was renamed to `refresh` in cached 1.0; use `refresh = ...`",
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

    if args.expires {
        if args.ttl.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `ttl` are mutually exclusive — `expires` delegates expiry to the value via the `Expires` trait",
            )
            .to_compile_error()
            .into();
        }
        if args.redis {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `redis` are mutually exclusive — `expires` selects sharded in-memory expiring stores",
            )
            .to_compile_error()
            .into();
        }
        if args.disk {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `disk` are mutually exclusive — `expires` selects sharded in-memory expiring stores",
            )
            .to_compile_error()
            .into();
        }
        if args.ty.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `ty` are mutually exclusive — `expires` generates the store type automatically",
            )
            .to_compile_error()
            .into();
        }
        if args.create.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `create` are mutually exclusive — `expires` generates the store constructor automatically",
            )
            .to_compile_error()
            .into();
        }
        if args.refresh.is_some() {
            return syn::Error::new(
                fn_ident.span(),
                "`expires` and `refresh` are mutually exclusive — `expires` delegates expiry to the value via `Expires::is_expired`",
            )
            .to_compile_error()
            .into();
        }
        if args.cache_none {
            return syn::Error::new(
                fn_ident.span(),
                "`expires = true` and `cache_none = true` are incompatible — `expires` requires \
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
                "`result_fallback = true` and `expires = true` are mutually exclusive — \
                 `expires` selects a per-value expiry store; `result_fallback` requires \
                 a fixed-TTL store whose entry expiry can be detected and refreshed by \
                 the cache layer, which per-value expiry does not support. \
                 Note: `ttl` and `expires` serve different purposes — `ttl` applies a fixed \
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
                "`expires = true` and `cache_err = true` are mutually exclusive — `expires` \
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

    // `is_smart_option`: skip None, cache Some(T) — default for Option<T> returns.
    // Opt out with `cache_none = true` to force caching None as well.
    // `is_smart_result`: cache only Ok(T), skip Err — always true for Result returns here;
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
             `with_cached_flag` wraps the `Ok` value in `Return<T>` — the generated \
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
    // branch and generate `result.value.clone()` on an `Option<Return<T>>` — a confusing
    // compile error with a bad span. Catch it here with a clear diagnostic.
    if args.with_cached_flag && !is_smart_option && is_option_return {
        return syn::Error::new(
            output_span,
            "`with_cached_flag = true` and `cache_none = true` are structurally incompatible \
             on `Option<T>` returns: `with_cached_flag` unwraps `Return<T>` and stores `T`, \
             while `cache_none = true` stores `Option<T>` as the cached value — the same \
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

        // The `Ok` type of the function's `Result<…, E>`.
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
            // structurally `Return<…>`; gating on `with_cached_flag` (rather
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
        Some(ref name) => Ident::new(name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };
    let cache_name = cache_ident.to_string();

    let (cache_key_ty, key_convert_block) =
        match make_cache_key_type(&args.key, &args.convert, &args.ty, input_tys, &input_names) {
            Ok(key) => key,
            Err(error) => return error.to_compile_error().into(),
        };

    // Track whether the cache uses Infallible errors (the in-memory sharded default).
    // When true, `map_error` is a compile error and cache ops use `.expect(…)`.
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

    if args.result_fallback && args.ttl.is_none() {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires `ttl` to be set (e.g. `ttl = 60`). It serves the last \
             cached `Ok` value when a refresh returns `Err`, but a refresh only happens after an \
             entry expires. Without a TTL entries never expire, so the function body is never \
             re-run for a cached key and the fallback can never fire — making the option a no-op. \
             Set a TTL so cached entries expire and `result_fallback` has something to fall back to.",
        )
        .to_compile_error()
        .into();
    }

    // Resolve the cache-error handling strategy.  For the default sharded
    // in-memory stores the error type is `Infallible`, so cache operations can
    // never fail and `.expect(…)` is always correct.  `map_error` is rejected on
    // this path — there are no errors to map.
    //
    // For the fallible redis / disk / custom paths the user must supply
    // `map_error = "…"` and we keep the original `.map_err(#map_error)?` pattern.
    let map_error_opt: Option<ExprClosure> = match (&args.map_error, infallible_default) {
        (Some(_), true) => {
            return syn::Error::new(
                fn_ident.span(),
                "`map_error` is not applicable to the default in-memory sharded stores — \
                 their error type is `Infallible` and cache operations cannot fail. \
                 Remove `map_error`, or add `redis = true`, `disk = true`, or a custom \
                 `ty`/`create` to use a store with a fallible error type.",
            )
            .to_compile_error()
            .into();
        }
        (Some(src), false) => match parse_str::<ExprClosure>(src) {
            Ok(map_error) => Some(map_error),
            Err(error) => {
                return syn::Error::new(
                    fn_ident.span(),
                    format!("unable to parse `map_error` closure: {error}"),
                )
                .to_compile_error()
                .into();
            }
        },
        (None, true) => None, // infallible: use .expect(…) in generated code
        (None, false) => {
            return syn::Error::new(
                fn_ident.span(),
                "#[concurrent_cached] requires `map_error = \"…\"` when the cache type \
                 has a fallible error (redis/disk/custom)",
            )
            .to_compile_error()
            .into();
        }
    };

    // Emit either `.map_err(closure)?` for fallible stores or `.expect(…)` for
    // infallible stores.  The macro helpers below use these token-stream fragments.
    //
    // `infallible_default` is true for the default in-memory sharded stores; their error
    // type is `Infallible`, so the ops always succeed and `.expect(…)` is correct.
    // `map_error` on this path is rejected above as a compile error.
    let (cache_get_unwrap, cache_set_unwrap): (proc_macro2::TokenStream, proc_macro2::TokenStream) =
        if infallible_default {
            // The store's error type is `Infallible`; these `.expect()`s are unreachable.
            let msg = "cache operation on the default in-memory sharded store is infallible";
            (quote! { .expect(#msg) }, quote! { .expect(#msg) })
        } else {
            let me = map_error_opt
                .as_ref()
                .expect("fallible path requires map_error (validated above)");
            (quote! { .map_err(#me)? }, quote! { .map_err(#me)? })
        };
    // For `await`-ed variants we need identical logic.
    let cache_get_unwrap_async = cache_get_unwrap.clone();
    let cache_set_unwrap_async = cache_set_unwrap.clone();

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = if with_cached_flag_result {
        // Result<Return<T>, E>: cache the inner T from Ok(Return<T>).
        (
            if asyncness.is_some() {
                quote! {
                    if let Ok(result) = &result {
                        ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.value.clone()).await #cache_set_unwrap_async;
                    }
                }
            } else {
                quote! {
                    if let Ok(result) = &result {
                        ::cached::ConcurrentCached::cache_set(cache, key, result.value.clone()) #cache_set_unwrap;
                    }
                }
            },
            quote! { let mut r = ::cached::Return::new(result); r.was_cached = true; return Ok(r) },
        )
    } else if with_cached_flag_option {
        // Option<Return<T>>: cache the inner T from Some(Return<T>), skip None.
        (
            if asyncness.is_some() {
                quote! {
                    if let Some(result) = &result {
                        ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.value.clone()).await #cache_set_unwrap_async;
                    }
                }
            } else {
                quote! {
                    if let Some(result) = &result {
                        ::cached::ConcurrentCached::cache_set(cache, key, result.value.clone()) #cache_set_unwrap;
                    }
                }
            },
            quote! { let mut r = ::cached::Return::new(result); r.was_cached = true; return Some(r) },
        )
    } else if args.with_cached_flag {
        // Plain Return<T>: cache the inner T directly.
        (
            if asyncness.is_some() {
                quote! {
                    ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.value.clone()).await #cache_set_unwrap_async;
                }
            } else {
                quote! {
                    ::cached::ConcurrentCached::cache_set(cache, key, result.value.clone()) #cache_set_unwrap;
                }
            },
            quote! { let mut r = ::cached::Return::new(result); r.was_cached = true; return r },
        )
    } else if is_smart_result {
        // Result<T, E> return type: cache only Ok(T), skip Err
        (
            if asyncness.is_some() {
                quote! {
                    if let Ok(result) = &result {
                        ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.clone()).await #cache_set_unwrap_async;
                    }
                }
            } else {
                quote! {
                    if let Ok(result) = &result {
                        ::cached::ConcurrentCached::cache_set(cache, key, result.clone()) #cache_set_unwrap;
                    }
                }
            },
            quote! { return Ok(result) },
        )
    } else if is_smart_option {
        // Option<T>: cache Some(T), skip None. infallible_default guaranteed.
        (
            if asyncness.is_some() {
                quote! {
                    if let Some(result) = &result {
                        ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.clone()).await #cache_set_unwrap_async;
                    }
                }
            } else {
                quote! {
                    if let Some(result) = &result {
                        ::cached::ConcurrentCached::cache_set(cache, key, result.clone()) #cache_set_unwrap;
                    }
                }
            },
            quote! { return Some(result) },
        )
    } else {
        // Plain return type — infallible_default is guaranteed true here.
        // No Ok/Err wrapping: the result is the value directly.
        (
            if asyncness.is_some() {
                quote! {
                    ::cached::ConcurrentCachedAsync::async_cache_set(cache, key, result.clone()).await #cache_set_unwrap_async;
                }
            } else {
                quote! {
                    ::cached::ConcurrentCached::cache_set(cache, key, result.clone()) #cache_set_unwrap;
                }
            },
            quote! { return result },
        )
    };

    // Clone the full original signature and rename it to `inner`. Quoting the
    // whole `syn::Signature` preserves the `where` clause (and lifetimes,
    // const generics, etc.) — `#generics` alone drops the where clause.
    let mut inner_sig = signature.clone();
    inner_sig.ident = Ident::new("inner", fn_ident.span());

    // `do_set_return_block`: runs `inner`, sets the cache, returns the result.
    // For `result_fallback`, the expiry-aware lookup (via `ConcurrentCloneCached`) is folded
    // into this block; no separate `_FALLBACK` static is needed.
    let do_set_return_block = if args.result_fallback && asyncness.is_some() {
        quote! {
            let cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            let old_val = {
                let (val, expired) = ::cached::ConcurrentCloneCached::cache_get_with_expiry_status(cache, &key);
                match (val, expired) {
                    (Some(result), false) => { #return_cache_block }
                    (stale, _) => stale,
                }
            };
            #inner_sig #body
            let result = inner(#(#input_names),*).await;
            let result = match (result.is_err(), old_val) {
                (true, Some(old_val)) => Ok(old_val),
                _ => result,
            };
            if let Ok(ok_val) = &result {
                ::cached::ConcurrentCachedAsync::async_cache_set(cache, key.clone(), ok_val.clone()).await #cache_set_unwrap_async;
            }
            result
        }
    } else if args.result_fallback {
        quote! {
            let cache = &*#cache_ident;
            let old_val = {
                let (val, expired) = ::cached::ConcurrentCloneCached::cache_get_with_expiry_status(cache, &key);
                match (val, expired) {
                    (Some(result), false) => { #return_cache_block }
                    (stale, _) => stale,
                }
            };
            #inner_sig #body
            let result = inner(#(#input_names),*);
            let result = match (result.is_err(), old_val) {
                (true, Some(old_val)) => Ok(old_val),
                _ => result,
            };
            if let Ok(ok_val) = &result {
                ::cached::ConcurrentCached::cache_set(cache, key.clone(), ok_val.clone()) #cache_set_unwrap;
            }
            result
        }
    } else if asyncness.is_some() {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*).await;
            let cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            #set_cache_block
            result
        }
    } else {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*);
            let cache = &*#cache_ident;
            #set_cache_block
            result
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
    // prime unconditionally reruns the function and stores the result — no old_val fallback,
    // no early-return on fresh hit. For all other paths, prime reuses `do_set_return_block`
    // which already implements "run inner and set cache".
    let prime_do_set_return_block = if args.result_fallback && asyncness.is_some() {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*).await;
            let cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
            if let Ok(ok_val) = &result {
                ::cached::ConcurrentCachedAsync::async_cache_set(cache, key.clone(), ok_val.clone()).await #cache_set_unwrap_async;
            }
            result
        }
    } else if args.result_fallback {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*);
            let cache = &*#cache_ident;
            if let Ok(ok_val) = &result {
                ::cached::ConcurrentCached::cache_set(cache, key.clone(), ok_val.clone()) #cache_set_unwrap;
            }
            result
        }
    } else {
        do_set_return_block.clone()
    };

    // `initial_cache_lookup`: the early-return guard block emitted at the start of the cached
    // function body. For `result_fallback`, the lookup is folded into `do_set_return_block`
    // (via `ConcurrentCloneCached`), so we emit nothing here for that path.
    let initial_cache_lookup_async = if args.result_fallback {
        quote! {}
    } else {
        quote! {
            {
                // check if the result is cached
                let cache = #cache_ident.get_or_init(|| async { #cache_create }).await;
                if let Some(result) = ::cached::ConcurrentCachedAsync::async_cache_get(cache, &key).await #cache_get_unwrap_async {
                    #return_cache_block
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
                let cache = &*#cache_ident;
                if let Some(result) = ::cached::ConcurrentCached::cache_get(cache, &key) #cache_get_unwrap {
                    #return_cache_block
                }
            }
        }
    };

    // put it all together
    let expanded = if asyncness.is_some() {
        quote! {
            // Cached static
            #[doc = #cache_ident_doc]
            #visibility static #cache_ident: ::cached::async_sync::OnceCell<#cache_ty> = ::cached::async_sync::OnceCell::const_new();
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                let key = #key_convert_block;
                #initial_cache_lookup_async
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                let key = #key_convert_block;
                #prime_do_set_return_block
            }
        }
    } else {
        quote! {
            // Cached static
            #[doc = #cache_ident_doc]
            #visibility static #cache_ident: ::std::sync::LazyLock<#cache_ty> = ::std::sync::LazyLock::new(|| #cache_create);
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                let key = #key_convert_block;
                #initial_cache_lookup_sync
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                let key = #key_convert_block;
                #prime_do_set_return_block
            }
        }
    };

    expanded.into()
}

fn get_redis_cache_type_and_create(
    args: &ConcurrentCachedArgs,
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
                quote! { cached::AsyncRedisCache<#cache_key_ty, #cache_value_ty> }
            } else {
                quote! { cached::RedisCache<#cache_key_ty, #cache_value_ty> }
            }
        }
    };
    let cache_create = match &args.create {
        Some(cache_create) => {
            check_create_conflicts(args, cache_ident.span())?;
            let cache_create = parse_str::<Block>(cache_create.as_ref()).map_err(|e| {
                syn::Error::new(
                    cache_ident.span(),
                    format!("unable to parse cache create block: {e}"),
                )
            })?;
            quote! { #cache_create }
        }
        None => {
            if let Some(ttl) = args.ttl {
                let cache_prefix = if let Some(cp) = &args.cache_prefix_block {
                    cp.to_string()
                } else {
                    format!(
                        " {{ \"cached::macros::concurrent_cached::{}\" }}",
                        cache_ident
                    )
                };
                let cache_prefix = parse_str::<Block>(cache_prefix.as_ref()).map_err(|e| {
                    syn::Error::new(
                        cache_ident.span(),
                        format!("unable to parse cache_prefix_block: {e}"),
                    )
                })?;
                match args.refresh {
                    Some(refresh) => {
                        if is_async {
                            quote! { cached::AsyncRedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).refresh_on_hit(#refresh).build().await.unwrap_or_else(|e| panic!("error constructing AsyncRedisCache in #[concurrent_cached] macro: {e}")) }
                        } else {
                            quote! {
                                cached::RedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("error constructing RedisCache in #[concurrent_cached] macro: {e}"))
                            }
                        }
                    }
                    None => {
                        if is_async {
                            quote! { cached::AsyncRedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).build().await.unwrap_or_else(|e| panic!("error constructing AsyncRedisCache in #[concurrent_cached] macro: {e}")) }
                        } else {
                            quote! {
                                cached::RedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).build().unwrap_or_else(|e| panic!("error constructing RedisCache in #[concurrent_cached] macro: {e}"))
                            }
                        }
                    }
                }
            } else if is_async {
                return Err(syn::Error::new(
                    cache_ident.span(),
                    "AsyncRedisCache requires a `ttl` when `create` block is not specified",
                ));
            } else {
                return Err(syn::Error::new(
                    cache_ident.span(),
                    "RedisCache requires a `ttl` when `create` block is not specified",
                ));
            }
        }
    };
    Ok((cache_ty, cache_create))
}

fn get_disk_cache_type_and_create(
    args: &ConcurrentCachedArgs,
    cache_name: &str,
    cache_key_ty: &proc_macro2::TokenStream,
    cache_value_ty: &proc_macro2::TokenStream,
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
            quote! { cached::RedbCache<#cache_key_ty, #cache_value_ty> }
        }
    };
    let cache_create = match &args.create {
        Some(cache_create) => {
            check_create_conflicts(args, fn_ident.span())?;
            let cache_create = parse_str::<Block>(cache_create.as_ref()).map_err(|e| {
                syn::Error::new(
                    fn_ident.span(),
                    format!("unable to parse cache create block: {e}"),
                )
            })?;
            quote! { #cache_create }
        }
        None => {
            let create = quote! {
                cached::RedbCache::new(#cache_name)
            };
            let create = match args.ttl {
                None => create,
                Some(ttl) => {
                    quote! {
                        (#create).ttl(::cached::time::Duration::from_secs(#ttl))
                    }
                }
            };
            let create = match args.refresh {
                None => create,
                Some(refresh) => {
                    quote! {
                        (#create).refresh_on_hit(#refresh)
                    }
                }
            };
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
/// |  no  |  no |   no    | `ShardedCache` |
/// | yes  |  no |   no    | `ShardedLruCache` |
/// |  no  | yes |   no    | `ShardedTtlCache`         (requires `time_stores` feature on `cached`) |
/// | yes  | yes |   no    | `ShardedLruTtlCache`      (requires `time_stores` feature on `cached`) |
/// |  no  |  —  |   yes   | `ShardedExpiringCache`    (per-value expiry; `ttl` is rejected with `expires`) |
/// | yes  |  —  |   yes   | `ShardedExpiringLruCache` (per-value expiry; `ttl` is rejected with `expires`) |
///
/// `shards = N` is honored on every variant and routes through the `_and_shards`
/// shortcut constructor.
fn get_sharded_cache_type_and_create(
    args: &ConcurrentCachedArgs,
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
    if matches!(args.ttl, Some(0)) {
        return Err(syn::Error::new(fn_ident.span(), "`ttl` must be >= 1"));
    }
    if args.refresh.is_some_and(|r| r) && args.ttl.is_none() {
        return Err(syn::Error::new(
            fn_ident.span(),
            "`refresh` requires `ttl` to be set on the default in-memory sharded path",
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
                let ty =
                    quote! { ::cached::ShardedExpiringLruCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { ::cached::ShardedExpiringLruCache::builder().max_size(#size).shards(#n).build().unwrap_or_else(|e| panic!("ShardedExpiringLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { ::cached::ShardedExpiringLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ShardedExpiringLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            None => {
                let ty = quote! { ::cached::ShardedExpiringCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { ::cached::ShardedExpiringCache::builder().shards(#n).build().unwrap_or_else(|e| panic!("ShardedExpiringCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { ::cached::ShardedExpiringCache::builder().build().unwrap_or_else(|e| panic!("ShardedExpiringCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
        }
    } else {
        match (args.max_size, args.ttl) {
            (None, None) => {
                let ty = quote! { ::cached::ShardedCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { ::cached::ShardedCache::builder().shards(#n).build().unwrap_or_else(|e| panic!("ShardedCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { ::cached::ShardedCache::builder().build().unwrap_or_else(|e| panic!("ShardedCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            (Some(size), None) => {
                let ty = quote! { ::cached::ShardedLruCache<#cache_key_ty, #cache_value_ty> };
                let create = match args.shards {
                    Some(n) => {
                        quote! { ::cached::ShardedLruCache::builder().max_size(#size).shards(#n).build().unwrap_or_else(|e| panic!("ShardedLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                    None => {
                        quote! { ::cached::ShardedLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ShardedLruCache build failed in #[concurrent_cached]: {e}")) }
                    }
                };
                (ty, create)
            }
            (None, Some(ttl)) => {
                let ty = quote! { ::cached::ShardedTtlCache<#cache_key_ty, #cache_value_ty> };
                let refresh = args.refresh.unwrap_or(false);
                let create = match args.shards {
                    Some(n) => quote! {{
                        let __c = ::cached::ShardedTtlCache::builder()
                            .ttl(::cached::time::Duration::from_secs(#ttl))
                            .shards(#n)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                    None => quote! {{
                        let __c = ::cached::ShardedTtlCache::builder()
                            .ttl(::cached::time::Duration::from_secs(#ttl))
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                };
                (ty, create)
            }
            (Some(size), Some(ttl)) => {
                let ty = quote! { ::cached::ShardedLruTtlCache<#cache_key_ty, #cache_value_ty> };
                let refresh = args.refresh.unwrap_or(false);
                let create = match args.shards {
                    Some(n) => quote! {{
                        let __c = ::cached::ShardedLruTtlCache::builder()
                            .max_size(#size)
                            .ttl(::cached::time::Duration::from_secs(#ttl))
                            .shards(#n)
                            .refresh_on_hit(#refresh)
                            .build()
                            .unwrap_or_else(|e| panic!("ShardedLruTtlCache build failed in #[concurrent_cached]: {e}"));
                        __c
                    }},
                    None => quote! {{
                        let __c = ::cached::ShardedLruTtlCache::builder()
                            .max_size(#size)
                            .ttl(::cached::time::Duration::from_secs(#ttl))
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
                "#[concurrent_cached] cache `ty` must be specified",
            ));
        }
    };
    let cache_create = match &args.create {
        Some(cache_create) => {
            check_create_conflicts(args, fn_ident.span())?;
            let cache_create = parse_str::<Block>(cache_create.as_ref()).map_err(|e| {
                syn::Error::new(
                    fn_ident.span(),
                    format!("unable to parse cache create block: {e}"),
                )
            })?;
            quote! { #cache_create }
        }
        None => {
            return Err(syn::Error::new(
                fn_ident.span(),
                "#[concurrent_cached] cache `create` block must be specified",
            ));
        }
    };
    Ok((cache_ty, cache_create))
}
