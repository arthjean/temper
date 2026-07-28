#[inline(never)]
fn profiled_function(value: u64) -> u64 {
    value.wrapping_mul(3).wrapping_add(1)
}

#[inline(never)]
fn missing_profile_function(value: u64) -> u64 {
    value.wrapping_mul(5).wrapping_add(2)
}

fn unrelated_dead_code_warning() {}

fn main() {
    println!(
        "{}",
        profiled_function(41).wrapping_add(missing_profile_function(7))
    );
}
