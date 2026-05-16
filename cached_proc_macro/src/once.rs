use crate::helpers::*;
use darling::ast::NestedMeta;
use darling::FromMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{parse_macro_input, Ident, ItemFn, ReturnType};

#[derive(FromMeta)]
struct OnceMacroArgs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    ttl: Option<u64>,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    sync_writes: SyncWriteMode,
    #[darling(default = "default_sync_writes_buckets")]
    sync_writes_buckets: usize,
    #[darling(default)]
    result: bool,
    #[darling(default)]
    option: bool,
    #[darling(default)]
    with_cached_flag: bool,
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
            "#[once] cannot be applied to methods that take `self`",
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

    let cache_value_ty = match find_value_type(args.result, args.option, &output, output_ty) {
        Ok(value_ty) => value_ty,
        Err(e) => return e.to_compile_error().into(),
    };

    // make the cache identifier
    let cache_ident = match args.name {
        Some(name) => Ident::new(&name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    if let Err(error) = validate_sync_writes_buckets(args.sync_writes_buckets, fn_ident.span()) {
        return error.to_compile_error().into();
    }
    if args.sync_writes == SyncWriteMode::ByKey {
        return syn::Error::new(
            fn_ident.span(),
            "`sync_writes = \"by_key\"` is not supported by `#[once]` because `#[once]` stores a single value for all arguments",
        )
        .to_compile_error()
        .into();
    }
    let sync_writes_buckets = args.sync_writes_buckets;

    // make the cache type and create statement
    let (cache_ty, cache_create) = match &args.ttl {
        None => (quote! { Option<#cache_value_ty> }, quote! { None }),
        Some(_) => (
            quote! { Option<(::cached::time::Instant, #cache_value_ty)> },
            quote! { None },
        ),
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (&args.result, &args.option) {
        (false, false) => {
            let set_cache_block = if args.ttl.is_some() {
                quote! {
                    *cached = Some((::cached::time::Instant::now(), result.clone()));
                }
            } else {
                quote! {
                    *cached = Some(result.clone());
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return r }
            } else {
                quote! { return result.clone() }
            };
            let return_cache_block = gen_return_cache_block(args.ttl, return_cache_block);
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = if args.ttl.is_some() {
                quote! {
                    if let Ok(result) = &result {
                        *cached = Some((::cached::time::Instant::now(), result.clone()));
                    }
                }
            } else {
                quote! {
                    if let Ok(result) = &result {
                        *cached = Some(result.clone());
                    }
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return Ok(r) }
            } else {
                quote! { return Ok(result.clone()) }
            };
            let return_cache_block = gen_return_cache_block(args.ttl, return_cache_block);
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = if args.ttl.is_some() {
                quote! {
                    if let Some(result) = &result {
                        *cached = Some((::cached::time::Instant::now(), result.clone()));
                    }
                }
            } else {
                quote! {
                    if let Some(result) = &result {
                        *cached = Some(result.clone());
                    }
                }
            };

            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return Some(r) }
            } else {
                quote! { return Some(result.clone()) }
            };
            let return_cache_block = gen_return_cache_block(args.ttl, return_cache_block);
            (set_cache_block, return_cache_block)
        }
        _ => {
            return syn::Error::new(
                fn_ident.span(),
                "`result` and `option` attributes are mutually exclusive",
            )
            .to_compile_error()
            .into();
        }
    };

    let set_cache_and_return = quote! {
        #set_cache_block
        result
    };

    // Clone the full original signature and rename it to `inner`. Quoting the
    // whole `syn::Signature` preserves the `where` clause (and lifetimes,
    // const generics, etc.) — `#generics` alone drops the where clause.
    let mut inner_sig = signature.clone();
    inner_sig.ident = Ident::new("inner", fn_ident.span());

    let r_lock;
    let w_lock;
    let function_call;
    let ty;
    if asyncness.is_some() {
        w_lock = quote! {
            // try to get a write lock
            let mut cached = #cache_ident.write().await;
        };

        r_lock = quote! {
            // try to get a read lock
            let cached = #cache_ident.read().await;
        };

        function_call = quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*).await;
        };

        ty = match args.sync_writes {
            SyncWriteMode::ByKey => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<(::cached::async_sync::RwLock<#cache_ty>, Vec<std::sync::Arc<::cached::async_sync::RwLock<()>>>)> = ::std::sync::LazyLock::new(|| (::cached::async_sync::RwLock::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(::cached::async_sync::RwLock::new(()))).collect()));
            },
            _ => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<::cached::async_sync::RwLock<#cache_ty>> = ::std::sync::LazyLock::new(|| ::cached::async_sync::RwLock::new(#cache_create));
            },
        };
    } else {
        w_lock = quote! {
            // try to get a lock first
            let mut cached = #cache_ident.write();
        };

        r_lock = quote! {
            // try to get a read lock
            let cached = #cache_ident.read();
        };

        function_call = quote! {
            #inner_sig #body
            let result = inner(#(#input_names),*);
        };

        ty = match args.sync_writes {
            SyncWriteMode::ByKey => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<(::cached::sync_sync::RwLock<#cache_ty>, Vec<std::sync::Arc<::cached::sync_sync::RwLock<()>>>)> = ::std::sync::LazyLock::new(|| (::cached::sync_sync::RwLock::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(::cached::sync_sync::RwLock::new(()))).collect()));
            },
            _ => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<::cached::sync_sync::RwLock<#cache_ty>> = ::std::sync::LazyLock::new(|| ::cached::sync_sync::RwLock::new(#cache_create));
            },
        };
    }

    let prime_do_set_return_block = match args.sync_writes {
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
            if let Some(result) = &*cached {
                #return_cache_block
            }
        }
    };

    let do_set_return_block = match args.sync_writes {
        SyncWriteMode::Default => quote! {
            #r_lock_return_cache_block
            #w_lock
            if let Some(result) = &*cached {
                #return_cache_block
            }
            #function_call
            #set_cache_and_return
        },
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
    let now_block = if args.ttl.is_some() {
        quote! { let now = ::cached::time::Instant::now(); }
    } else {
        quote! {}
    };

    let expanded = quote! {
        // Cached static
        #[doc = #cache_ident_doc]
        #ty
        // Cached function
        #(#attributes)*
        #visibility #signature_no_muts {
            #now_block
            #do_set_return_block
        }
        // Prime cached function
        #[doc = #prime_fn_indent_doc]
        #[allow(dead_code)]
        #visibility #prime_sig {
            #now_block
            #prime_do_set_return_block
        }
    };

    expanded.into()
}
