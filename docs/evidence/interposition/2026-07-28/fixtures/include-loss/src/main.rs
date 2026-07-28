// EP-001 US-001 fixture. The sentinel `--cfg temper_included_sentinel` is
// defined only through a Cargo configuration `include` file, so the binary
// reports whether the effective compiler input survived the build that produced
// it. The workload keeps the process long enough to yield PGO profile data.

const ROUNDS: u64 = 40_000_000;

#[inline(never)]
fn mix(seed: u64) -> u64 {
    let mut value = seed ^ 0x9e37_79b9_7f4a_7c15;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(temper_included_sentinel)]
const SENTINEL: &str = "present";

#[cfg(not(temper_included_sentinel))]
const SENTINEL: &str = "absent";

fn main() {
    let mut accumulator = 0_u64;
    for round in 0..ROUNDS {
        accumulator = accumulator.wrapping_add(mix(round ^ accumulator));
    }
    println!("sentinel={SENTINEL} accumulator={accumulator}");
}
