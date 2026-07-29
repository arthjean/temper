#[cfg(not(temper_target))]
compile_error!("target rustflags did not reach the selected binary");

fn main() {
    println!(
        "{}",
        temper_macro::answer!() + pgo_workspace_support::support()
    );
}

