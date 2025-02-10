extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;

use cealn_core::fs::FilenameSemantics;

/// Allows testing some functionality against a set of platform-specific filesystems
///
/// Various platforms have different sets of filesystems with different behavior, particularly around filename case
/// and normalization. This attribute creates copies of a particular test that creates mounted images for each of these
/// types of filesystems so they can all be covered by the test. In doing so, it also removes an implicit dependency of
/// the test on the filesystem type the user is running on.
#[proc_macro_attribute]
pub fn fs_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);

    let attrs = &input.attrs;
    let impl_block = &input.block;

    let impl_sig = input.sig.clone();
    let impl_ident = &impl_sig.ident;

    let mut instances = Vec::new();

    for filename_semantics in [
        FilenameSemantics::GenericPosix,
        FilenameSemantics::Ntfs { win32_semantics: true },
        FilenameSemantics::HfsPlus { case_sensitive: true },
        FilenameSemantics::HfsPlus { case_sensitive: false },
        FilenameSemantics::Apfs { case_sensitive: true },
        FilenameSemantics::Apfs { case_sensitive: false },
    ]
    .iter()
    {
        let semantics_ident = match filename_semantics {
            FilenameSemantics::GenericPosix => "posix",
            FilenameSemantics::Ntfs { win32_semantics: true } => "ntfs_win32",
            FilenameSemantics::Ntfs { win32_semantics: false } => "ntfs_raw",
            FilenameSemantics::HfsPlus { case_sensitive: true } => "hfsplus_case_sens",
            FilenameSemantics::HfsPlus { case_sensitive: false } => "hfsplus_case_insens",
            FilenameSemantics::Apfs { case_sensitive: true } => "apfs_case_sens",
            FilenameSemantics::Apfs { case_sensitive: false } => "apfs_case_insens",
        };

        let quote_semantics = match filename_semantics {
            FilenameSemantics::GenericPosix => quote! { ::cealn_core::fs::FilenameSemantics::GenericPosix },
            FilenameSemantics::Ntfs { win32_semantics } => {
                quote! { ::cealn_core::fs::FilenameSemantics::Ntfs { win32_semantics: #win32_semantics } }
            }
            FilenameSemantics::HfsPlus { case_sensitive } => {
                quote! { ::cealn_core::fs::FilenameSemantics::HfsPlus { case_sensitive: #case_sensitive } }
            }
            FilenameSemantics::Apfs { case_sensitive } => {
                quote! { ::cealn_core::fs::FilenameSemantics::Apfs { case_sensitive: #case_sensitive } }
            }
        };

        let target_condition = match filename_semantics {
            FilenameSemantics::GenericPosix => quote! { #[cfg(all(unix, not(target_os = "macos")))] },
            FilenameSemantics::Ntfs { .. } => quote! { #[cfg(target_os = "windows")] },
            FilenameSemantics::HfsPlus { .. } | FilenameSemantics::Apfs { .. } => {
                quote! { #[cfg(target_os = "macos")] }
            }
        };

        let instance_ident = syn::Ident::new(
            &format!("{}_{}", impl_sig.ident, semantics_ident),
            impl_sig.ident.span(),
        );

        let instance = quote! {
            #target_condition
            #(#attrs)*
            fn #instance_ident()
            {
                let temp_fs = ::cealn_test_util::fs::SharedTestFs::new(#quote_semantics).unwrap();
                #impl_ident(#quote_semantics, temp_fs.path().to_owned());
            }
        };

        instances.push(instance);
    }

    let result = quote! {
        #impl_sig #impl_block

        #(#instances)*
    };

    result.into()
}
