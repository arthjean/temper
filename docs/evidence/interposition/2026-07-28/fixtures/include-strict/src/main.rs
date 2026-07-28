// EP-001 US-001 behavioural control. This fixture cannot compile at all unless
// the target rustflag defined by the Cargo configuration `include` file reaches
// rustc, so a lost compiler input becomes an executed build failure rather than
// an inferred one.

#[cfg(not(temper_included_sentinel))]
compile_error!("the included target rustflag did not reach this compilation");

const ROUNDS: u64 = 40_000_000;

#[inline(never)]
fn mix(seed: u64) -> u64 {
    let mut value = seed ^ 0x9e37_79b9_7f4a_7c15;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn main() {
    let mut accumulator = 0_u64;
    for round in 0..ROUNDS {
        accumulator = accumulator.wrapping_add(mix(round ^ accumulator));
    }
    println!("strict accumulator={accumulator}");
}
