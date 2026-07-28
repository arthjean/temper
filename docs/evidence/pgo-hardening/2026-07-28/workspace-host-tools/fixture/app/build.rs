#[cfg(temper_target)]
compile_error!("target PGO rustflags reached a host build script");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}

