#[cfg(not(temper_included_sentinel))]
compile_error!("the included target rustflag did not reach this compilation");

fn main() {
    println!("included sentinel");
}
