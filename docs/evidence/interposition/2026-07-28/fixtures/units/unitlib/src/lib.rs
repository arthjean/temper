#[inline(never)]
pub fn mix(seed: u64) -> u64 {
    let mut value = seed ^ 0x9e37_79b9_7f4a_7c15;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
