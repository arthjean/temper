#[cfg(temper_target)]
compile_error!("target PGO rustflags reached a host proc macro");

use proc_macro::TokenStream;

#[proc_macro]
pub fn answer(_input: TokenStream) -> TokenStream {
    "42_u64".parse().expect("literal token stream")
}

