#[inline(never)]
fn profiled_function(value: u64) -> u64 {
    value.wrapping_mul(3).wrapping_add(1)
}

fn main() {
    println!("{}", profiled_function(41));
}

