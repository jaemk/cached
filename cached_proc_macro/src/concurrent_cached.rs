use crate::helpers::*;
use darling::ast::NestedMeta;
use darling::FromMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, parse_str, Block, Expr, ExprClosure, GenericArgument, Ident, ItemFn,
    ReturnType, Type,
};

#[derive(FromMeta)]
struct IOMacroArgs {
    map_error: String,
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
    #[darling(default)]
    refresh: Option<bool>,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<String>,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<String>,
    #[darling(default)]
    sync_to_disk_on_cache_change: Option<bool>,
    #[darling(default)]
    connection_config: Option<String>,
}

/// When a `create` block is supplied the user fully constructs the store, so
/// every store-builder attribute the macro would otherwise apply is dropped.
/// Reject those attributes with a precise message instead of silently ignoring
/// them — otherwise `disk_dir` / `sync_to_disk_on_cache_change` /
/// `connection_config` (and `ttl` / `refresh` / `cache_prefix_block`) look
/// applied but are not.
fn check_create_conflicts(args: &IOMacroArgs, span: proc_macro2::Span) -> Result<(), syn::Error> {
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
    if args.connection_config.is_some() {
        conflicting.push("connection_config");
    }
    if args.sync_to_disk_on_cache_change.is_some() {
        conflicting.push("sync_to_disk_on_cache_change");
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

pub fn concurrent_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    let args = match IOMacroArgs::from_list(&attr_args) {
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
                    |    `Result<cached::Return<T>, E>`\n\
                    Found type: {t}.",
                t = output_type_display
            ),
        )
        .to_compile_error()
        .into();
    }

    // Find the type of the value to store.
    // Return type always needs to be a result, so we want the (first) inner type.
    // For Result<i32, String>, store i32, etc.
    let cache_value_ty = {
        let ReturnType::Type(_, ty) = output.clone() else {
            return syn::Error::new(
                output_span,
                format!(
                    "#[concurrent_cached] functions must return `Result`s, found {output_type_display:?}"
                ),
            )
            .to_compile_error()
            .into();
        };

        // The outer type must be a `Result` — the generated body calls
        // `.map_err(#map_error)?` on it. Verify structurally so `-> Option<T>`,
        // `-> Vec<T>`, `-> T`, etc. fail here with a clear message instead of
        // deeper inside the generated code. A proc macro only sees tokens, so a
        // `Result` *type alias* renamed away from `Result` is not recognized
        // (the same token-only limitation documented for `with_cached_flag`).
        let is_result = matches!(
            &*ty,
            Type::Path(tp) if tp.path.segments.last().is_some_and(|s| s.ident == "Result")
        );
        if !is_result {
            return syn::Error::new(
                output_span,
                format!(
                    "#[concurrent_cached] functions must return `Result`s, found {output_type_display:?}"
                ),
            )
            .to_compile_error()
            .into();
        }

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
            let unable = format!(
                "#[concurrent_cached] unable to determine cache value type, found {output_type_display:?}"
            );
            let GenericArgument::Type(return_ty) = ok_ty else {
                return syn::Error::new(output_span, unable)
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
    };

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

    // make the cache type and create statement
    let (cache_ty, cache_create) = match (&args.redis, &args.disk) {
        (true, false) => match get_redis_cache_type_and_create(
            &args,
            &cache_ident,
            &cache_key_ty,
            &cache_value_ty,
            asyncness.is_some(),
        ) {
            Ok(v) => v,
            Err(e) => return e.to_compile_error().into(),
        },
        (false, true) => {
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
        _ => match get_custom_cache_type_and_create(&args, &fn_ident) {
            Ok(v) => v,
            Err(e) => return e.to_compile_error().into(),
        },
    };

    let map_error = &args.map_error;
    let map_error = match parse_str::<ExprClosure>(map_error) {
        Ok(map_error) => map_error,
        Err(error) => {
            return syn::Error::new(
                fn_ident.span(),
                format!("unable to parse `map_error` closure: {error}"),
            )
            .to_compile_error()
            .into();
        }
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = {
        let (set_cache_block, return_cache_block) = if args.with_cached_flag {
            (
                if asyncness.is_some() {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.value.clone()).await.map_err(#map_error)?;
                        }
                    }
                } else {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.value.clone()).map_err(#map_error)?;
                        }
                    }
                },
                quote! { let mut r = ::cached::Return::new(result.clone()); r.was_cached = true; return Ok(r) },
            )
        } else {
            (
                if asyncness.is_some() {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.clone()).await.map_err(#map_error)?;
                        }
                    }
                } else {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.clone()).map_err(#map_error)?;
                        }
                    }
                },
                quote! { return Ok(result.clone()) },
            )
        };
        (set_cache_block, return_cache_block)
    };

    // Clone the full original signature and rename it to `inner`. Quoting the
    // whole `syn::Signature` preserves the `where` clause (and lifetimes,
    // const generics, etc.) — `#generics` alone drops the where clause.
    let mut inner_sig = signature.clone();
    inner_sig.ident = Ident::new("inner", fn_ident.span());

    let do_set_return_block = if asyncness.is_some() {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*).await;
            let cache = &#cache_ident.get_or_init(init).await;
            #set_cache_block
            result
        }
    } else {
        quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*);
            let cache = &#cache_ident;
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

    let async_trait = if asyncness.is_some() {
        quote! {
            use cached::ConcurrentCachedAsync;
        }
    } else {
        quote! {
            use cached::ConcurrentCached;
        }
    };

    let async_cache_get_return = if asyncness.is_some() {
        quote! {
            if let Some(result) = cache.cache_get(&key).await.map_err(#map_error)? {
                #return_cache_block
            }
        }
    } else {
        quote! {
            if let Some(result) = cache.cache_get(&key).map_err(#map_error)? {
                #return_cache_block
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
                let init = || async { #cache_create };
                #async_trait
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let cache = &#cache_ident.get_or_init(init).await;
                    #async_cache_get_return
                }
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                #async_trait
                let init = || async { #cache_create };
                let key = #key_convert_block;
                #do_set_return_block
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
                use cached::ConcurrentCached;
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let cache = &#cache_ident;
                    if let Some(result) = cache.cache_get(&key).map_err(#map_error)? {
                        #return_cache_block
                    }
                }
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                use cached::ConcurrentCached;
                let key = #key_convert_block;
                #do_set_return_block
            }
        }
    };

    expanded.into()
}

fn get_redis_cache_type_and_create(
    args: &IOMacroArgs,
    cache_ident: &Ident,
    cache_key_ty: &proc_macro2::TokenStream,
    cache_value_ty: &proc_macro2::TokenStream,
    is_async: bool,
) -> Result<(proc_macro2::TokenStream, proc_macro2::TokenStream), syn::Error> {
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
                            quote! { cached::AsyncRedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).refresh(#refresh).build().await.expect("error constructing AsyncRedisCache in #[concurrent_cached] macro") }
                        } else {
                            quote! {
                                cached::RedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).refresh(#refresh).build().expect("error constructing RedisCache in #[concurrent_cached] macro")
                            }
                        }
                    }
                    None => {
                        if is_async {
                            quote! { cached::AsyncRedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).build().await.expect("error constructing AsyncRedisCache in #[concurrent_cached] macro") }
                        } else {
                            quote! {
                                cached::RedisCache::new(#cache_prefix, ::cached::time::Duration::from_secs(#ttl)).build().expect("error constructing RedisCache in #[concurrent_cached] macro")
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
    args: &IOMacroArgs,
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
            quote! { cached::DiskCache<#cache_key_ty, #cache_value_ty> }
        }
    };
    let connection_config = match &args.connection_config {
        Some(connection_config) => {
            let connection_config = parse_str::<Expr>(connection_config).map_err(|e| {
                syn::Error::new(
                    fn_ident.span(),
                    format!("unable to parse connection_config block: {e}"),
                )
            })?;
            Some(quote! { #connection_config })
        }
        None => None,
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
                cached::DiskCache::new(#cache_name)
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
                        (#create).refresh(#refresh)
                    }
                }
            };
            let create = match args.sync_to_disk_on_cache_change {
                None => create,
                Some(sync_to_disk_on_cache_change) => {
                    quote! {
                        (#create).sync_to_disk_on_cache_change(#sync_to_disk_on_cache_change)
                    }
                }
            };
            let create = match connection_config {
                None => create,
                Some(connection_config) => {
                    quote! {
                        (#create).connection_config(#connection_config)
                    }
                }
            };
            let create = match &args.disk_dir {
                None => create,
                Some(disk_dir) => {
                    quote! { (#create).disk_directory(#disk_dir) }
                }
            };
            quote! { (#create).build().expect("error constructing DiskCache in #[concurrent_cached] macro") }
        }
    };
    Ok((cache_ty, cache_create))
}

fn get_custom_cache_type_and_create(
    args: &IOMacroArgs,
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
