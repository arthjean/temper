// EP-001 US-002 fixture. The workspace contains one Cargo probe surface, one
// build script, one proc macro, one target dependency and one selected binary
// so a single build exercises every rustc invocation class Temper must
// classify.

const ROUNDS: u64 = 40_000_000;

fn main() {
    let mut accumulator = temper_units_macro::seed!();
    for round in 0..ROUNDS {
        accumulator = accumulator.wrapping_add(temper_units_lib::mix(round ^ accumulator));
    }
    println!("units accumulator={accumulator}");
}
