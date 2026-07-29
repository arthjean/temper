#[cfg(not(temper_target))]
compile_error!("target rustflags did not reach a target dependency");

pub fn support() -> u64 {
    1
}
