mod cached;
mod helpers;
mod io_cached;
mod once;

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    cached::cached(args, input)
}

#[proc_macro_attribute]
pub fn once(args: TokenStream, input: TokenStream) -> TokenStream {
    once::once(args, input)
}

#[proc_macro_attribute]
pub fn io_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    io_cached::io_cached(args, input)
}
