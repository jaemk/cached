use darling::{Error, FromMeta};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::__private::Span;
use quote::quote;
use std::ops::Deref;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    Attribute, Block, FnArg, GenericArgument, Pat, PatType, PathArguments, ReturnType, Signature,
    Type, parse_quote, parse_str,
};

/// Returns `true` if `output` is a `Result<…>` type (last path segment is
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

/// Returns `true` if `output` is an `Option<…>` type (last path segment is `"Option"` with type args).
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

#[derive(Debug, Default, Eq, PartialEq)]
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
            "the `result` and `option` attributes are mutually exclusive",
        )),
        _ => match output.clone() {
            ReturnType::Default => Err(syn::Error::new(
                output_ty.span(),
                "function must return something when `result` or `option` is set",
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
/// segment — e.g. the `T` in `Result<T, E>` or `Return<T>`. `not_path` is the
/// error message when `ty` is not a simple path type; `no_arg` is the message
/// when the path has no usable `<…>` argument. Used by `#[concurrent_cached]`
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
    convert: &Option<String>,
    ty: &Option<String>,
    input_tys: Vec<Type>,
    input_names: &Vec<Pat>,
) -> Result<(TokenStream2, TokenStream2), syn::Error> {
    match (key, convert, ty) {
        (Some(key_str), Some(convert_str), _) => {
            let cache_key_ty = parse_str::<Type>(key_str)?;

            let key_convert_block = parse_str::<Block>(convert_str)?;

            Ok((quote! {#cache_key_ty}, quote! {#key_convert_block}))
        }
        (None, Some(convert_str), Some(_)) => {
            let key_convert_block = parse_str::<Block>(convert_str)?;

            Ok((quote! {}, quote! {#key_convert_block}))
        }
        (None, None, _) => Ok((
            quote! {(#(#input_tys),*)},
            quote! {(#(#input_names.clone()),*)},
        )),
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
        .map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => *match_pattern_type(&pat_type),
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
        .map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => *pat_type.ty.clone(),
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

pub(super) fn gen_return_cache_block(
    time: Option<u64>,
    expires: bool,
    return_cache_block: TokenStream2,
) -> TokenStream2 {
    if expires {
        quote! {
            if !<_ as ::cached::Expires>::is_expired(result) {
                #return_cache_block
            }
        }
    } else if let Some(time) = &time {
        quote! {
            let (created_sec, result) = result;
            if now.saturating_duration_since(*created_sec) < ::cached::time::Duration::from_secs(#time) {
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
// (e.g. `use cached::Return as R;`) — but it correctly rejects an unrelated
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
