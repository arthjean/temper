fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(temper_units_build_script)");
    println!("cargo::rustc-cfg=temper_units_build_script");
}
