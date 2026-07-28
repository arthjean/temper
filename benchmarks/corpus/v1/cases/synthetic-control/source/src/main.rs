use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(operation) = arguments.next() else {
        eprintln!("missing operation");
        return ExitCode::FAILURE;
    };
    let Some(iterations) = arguments
        .next()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        eprintln!("missing or invalid iteration count");
        return ExitCode::FAILURE;
    };
    if arguments.next().is_some() {
        eprintln!("unexpected argument");
        return ExitCode::FAILURE;
    }

    let result = match operation.as_str() {
        "mix" => mix(iterations),
        "rotate" => rotate(iterations),
        _ => {
            eprintln!("unknown operation");
            return ExitCode::FAILURE;
        }
    };
    println!("{result:016x}");
    ExitCode::SUCCESS
}

fn mix(iterations: u64) -> u64 {
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    for index in 0..iterations {
        state ^= index.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        state = state.rotate_left(17).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        std::hint::black_box(state);
    }
    state
}

fn rotate(iterations: u64) -> u64 {
    let mut state = 0x1319_8a2e_0370_7344_u64;
    for index in 0..iterations {
        state = state
            .wrapping_add(index ^ 0xa409_3822_299f_31d0)
            .rotate_right((index % 63 + 1) as u32);
        std::hint::black_box(state);
    }
    state
}
