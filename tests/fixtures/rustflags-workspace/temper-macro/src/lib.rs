use proc_macro::TokenStream;

#[proc_macro]
pub fn answer(_input: TokenStream) -> TokenStream {
    "41 + 1".parse().expect("valid fixture expression")
}
