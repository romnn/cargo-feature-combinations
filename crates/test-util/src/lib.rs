//! Test attribute for this workspace's tests.

use proc_macro::TokenStream;
use quote::quote;

/// Marks a test that installs the [`color_eyre`] report handler before its body runs.
///
/// The function becomes an ordinary `#[test]`, so `#[should_panic]`, `#[ignore]` and a
/// `Result` return type work as usual.
/// The first test of a binary installs the handler; later installs fail and are ignored.
/// The crate using the attribute must depend on `color-eyre`.
///
/// [`color_eyre`]: https://docs.rs/color-eyre
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = proc_macro2::TokenStream::from(args);
    if !args.is_empty() {
        return syn::Error::new_spanned(args, "`test_util::test` takes no arguments")
            .to_compile_error()
            .into();
    }
    let mut function = syn::parse_macro_input!(item as syn::ItemFn);
    let body = &function.block;
    function.block = syn::parse_quote!({
        let _ = ::color_eyre::install();
        #body
    });
    quote! {
        #[::core::prelude::v1::test]
        #function
    }
    .into()
}
