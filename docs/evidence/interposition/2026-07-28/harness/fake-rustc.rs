// Temper v0.0.3 EP-001 experiment: a deterministic stand-in for the real
// compiler. It exists to prove byte transparency and Unix exit/signal semantics
// through the shim without depending on rustc behaviour.
//
//   TEMPER_EXP_FAKE_MODE      `ok` (default) | `exit42` | `abort`
//   TEMPER_EXP_FAKE_ARGV_OUT  optional path receiving length-framed raw argv
//   TEMPER_EXP_FAKE_STDOUT    optional literal written to stdout before echoing
//
// In `ok` mode the process copies stdin to stdout and writes one marker line to
// stderr, so a caller can prove all three standard streams survive `exec`.

use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;

fn main() {
    let arguments: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();

    if let Some(path) = env::var_os("TEMPER_EXP_FAKE_ARGV_OUT") {
        let mut framed = Vec::new();
        framed.extend_from_slice(&(arguments.len() as u64).to_be_bytes());
        for argument in &arguments {
            let bytes = argument.as_os_str().as_bytes();
            framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            framed.extend_from_slice(bytes);
        }
        let mut file = File::create(path).expect("create fake rustc argv record");
        file.write_all(&framed).expect("write fake rustc argv record");
    }

    match env::var("TEMPER_EXP_FAKE_MODE").as_deref().unwrap_or("ok") {
        "exit42" => std::process::exit(42),
        "abort" => std::process::abort(),
        _ => {}
    }

    if let Some(literal) = env::var_os("TEMPER_EXP_FAKE_STDOUT") {
        std::io::stdout()
            .write_all(literal.as_os_str().as_bytes())
            .expect("write fake rustc stdout literal");
    }
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read fake rustc stdin");
    std::io::stdout()
        .write_all(&input)
        .expect("echo fake rustc stdin");
    std::io::stdout().flush().expect("flush fake rustc stdout");
    eprintln!("fake-rustc-stderr-marker");
}
