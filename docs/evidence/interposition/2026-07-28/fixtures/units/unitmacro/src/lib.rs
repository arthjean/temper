use proc_macro::TokenStream;

#[proc_macro]
pub fn seed(_input: TokenStream) -> TokenStream {
    "0x5eed_u64".parse().expect("valid fixture expression")
}
