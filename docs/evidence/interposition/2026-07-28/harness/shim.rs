// Temper v0.0.3 EP-001 experiment shim.
//
// This file is evidence-harness code, not Temper production code. It is
// compiled directly with `rustc` (no Cargo, no dependency, no lockfile change)
// and installed as the `RUSTC` value of an experiment build.
//
// Contract (experiment protocol `temper-exp-shim-1`, environment only):
//   TEMPER_EXP_REAL_RUSTC   absolute path of the real compiler (required)
//   TEMPER_EXP_CAPTURE_DIR  directory receiving one record per invocation (required)
//   TEMPER_EXP_STAGE        stage label recorded in every record (required)
//   TEMPER_EXP_TARGET       exact supported target triple (required)
//   TEMPER_EXP_INJECT       `none` | `generate=<abs>` | `use=<abs>` (default `none`)
//
// The shim never parses argv through a shell, never converts arguments through
// UTF-8, writes exactly one create-new record before replacing itself, and
// replaces itself with the real compiler through Unix `exec`.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const PROTOCOL: &str = "temper-exp-shim-1";
const PROTOCOL_FAILURE_EXIT: i32 = 97;
const MISSING_FUNCTION_WARNING: &str = "-Cllvm-args=-pgo-warn-missing-function";
const OBSERVED_ENVIRONMENT: [&str; 6] = [
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_PRIMARY_PACKAGE",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
];

fn main() {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let real_rustc = required_path("TEMPER_EXP_REAL_RUSTC");
    let capture_directory = required_path("TEMPER_EXP_CAPTURE_DIR");
    let stage = required_string("TEMPER_EXP_STAGE");
    let target = required_string("TEMPER_EXP_TARGET");
    let injection = injection_mode();

    if !real_rustc.is_absolute() {
        protocol_failure("TEMPER_EXP_REAL_RUSTC must be an absolute path");
    }
    if !capture_directory.is_dir() {
        protocol_failure("TEMPER_EXP_CAPTURE_DIR must be an existing directory");
    }

    let classification = classify(&arguments, &target);
    let injected: Vec<String> = match (&injection, classification.class) {
        (Injection::None, _) => Vec::new(),
        (_, Class::Probe | Class::HostCompile | Class::Other) => Vec::new(),
        (Injection::Generate(path), Class::TargetCompile) => {
            vec![format!("-Cprofile-generate={path}")]
        }
        (Injection::Use(path), Class::TargetCompile) => vec![
            format!("-Cprofile-use={path}"),
            MISSING_FUNCTION_WARNING.to_owned(),
        ],
    };

    let mut final_arguments = arguments.clone();
    for flag in &injected {
        final_arguments.push(OsString::from(flag));
    }

    write_record(
        &capture_directory,
        &Record {
            stage: &stage,
            target: &target,
            real_rustc: &real_rustc,
            arguments: &arguments,
            final_arguments: &final_arguments,
            classification: &classification,
            injected: &injected,
        },
    );

    let mut command = Command::new(&real_rustc);
    command.args(&final_arguments);
    for name in [
        "TEMPER_EXP_REAL_RUSTC",
        "TEMPER_EXP_CAPTURE_DIR",
        "TEMPER_EXP_STAGE",
        "TEMPER_EXP_TARGET",
        "TEMPER_EXP_INJECT",
    ] {
        command.env_remove(name);
    }
    let error = command.exec();
    protocol_failure(&format!(
        "could not replace the shim with {}: {error}",
        real_rustc.display()
    ));
}

enum Injection {
    None,
    Generate(String),
    Use(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    TargetCompile,
    HostCompile,
    Probe,
    Other,
}

impl Class {
    fn label(self) -> &'static str {
        match self {
            Self::TargetCompile => "target_compile",
            Self::HostCompile => "host_compile",
            Self::Probe => "probe",
            Self::Other => "other",
        }
    }
}

struct Classification {
    class: Class,
    exact_target: bool,
    emit: bool,
    print: bool,
    crate_name: Option<String>,
    crate_types: Vec<String>,
    metadata: Option<String>,
    extra_filename: Option<String>,
    out_dir: Option<String>,
}

fn classify(arguments: &[OsString], target: &str) -> Classification {
    let mut exact_target = false;
    let mut emit = false;
    let mut print = false;
    let mut crate_name = None;
    let mut crate_types = Vec::new();
    let mut metadata = None;
    let mut extra_filename = None;
    let mut out_dir = None;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_os_str();
        let next = arguments.get(index + 1).map(OsString::as_os_str);
        if argument == OsStr::new("--target") {
            exact_target |= next.is_some_and(|value| value == OsStr::new(target));
        } else if let Some(value) = suffix(argument, "--target=") {
            exact_target |= value == target;
        } else if argument == OsStr::new("--emit") || starts_with(argument, "--emit=") {
            emit = true;
        } else if argument == OsStr::new("--print") || starts_with(argument, "--print=") {
            print = true;
        } else if argument == OsStr::new("--crate-name") {
            crate_name = next.and_then(lossless);
        } else if let Some(value) = suffix(argument, "--crate-name=") {
            crate_name = Some(value.to_owned());
        } else if argument == OsStr::new("--crate-type") {
            crate_types.extend(next.and_then(lossless));
        } else if let Some(value) = suffix(argument, "--crate-type=") {
            crate_types.push(value.to_owned());
        } else if argument == OsStr::new("--out-dir") {
            out_dir = next.and_then(lossless);
        } else if let Some(value) = suffix(argument, "--out-dir=") {
            out_dir = Some(value.to_owned());
        } else if let Some(value) = codegen_option(argument, next, "metadata=") {
            metadata = Some(value);
        } else if let Some(value) = codegen_option(argument, next, "extra-filename=") {
            extra_filename = Some(value);
        }
        index += 1;
    }

    let class = if print {
        Class::Probe
    } else if exact_target && emit {
        Class::TargetCompile
    } else if emit {
        Class::HostCompile
    } else {
        Class::Other
    };

    Classification {
        class,
        exact_target,
        emit,
        print,
        crate_name,
        crate_types,
        metadata,
        extra_filename,
        out_dir,
    }
}

fn codegen_option(argument: &OsStr, next: Option<&OsStr>, key: &str) -> Option<String> {
    if argument == OsStr::new("-C") {
        return next.and_then(lossless).and_then(|value| {
            value
                .strip_prefix(key)
                .map(str::to_owned)
        });
    }
    suffix(argument, "-C")
        .and_then(|value| value.strip_prefix(key))
        .map(str::to_owned)
}

fn starts_with(argument: &OsStr, prefix: &str) -> bool {
    argument.as_bytes().starts_with(prefix.as_bytes())
}

fn suffix<'a>(argument: &'a OsStr, prefix: &str) -> Option<&'a str> {
    if !starts_with(argument, prefix) {
        return None;
    }
    std::str::from_utf8(&argument.as_bytes()[prefix.len()..]).ok()
}

fn lossless(value: &OsStr) -> Option<String> {
    std::str::from_utf8(value.as_bytes()).ok().map(str::to_owned)
}

struct Record<'a> {
    stage: &'a str,
    target: &'a str,
    real_rustc: &'a PathBuf,
    arguments: &'a [OsString],
    final_arguments: &'a [OsString],
    classification: &'a Classification,
    injected: &'a [String],
}

fn write_record(directory: &PathBuf, record: &Record<'_>) {
    let mut json = String::new();
    json.push_str("{\"protocol\":");
    push_json_string(&mut json, PROTOCOL);
    json.push_str(",\"stage\":");
    push_json_string(&mut json, record.stage);
    json.push_str(",\"target\":");
    push_json_string(&mut json, record.target);
    json.push_str(",\"real_rustc\":");
    push_json_string(&mut json, &record.real_rustc.to_string_lossy());
    json.push_str(",\"cwd\":");
    push_json_string(
        &mut json,
        &env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    json.push_str(",\"pid\":");
    json.push_str(&std::process::id().to_string());
    json.push_str(",\"class\":");
    push_json_string(&mut json, record.classification.class.label());
    json.push_str(",\"exact_target\":");
    json.push_str(bool_literal(record.classification.exact_target));
    json.push_str(",\"emit\":");
    json.push_str(bool_literal(record.classification.emit));
    json.push_str(",\"print\":");
    json.push_str(bool_literal(record.classification.print));
    json.push_str(",\"crate_name\":");
    push_optional_string(&mut json, record.classification.crate_name.as_deref());
    json.push_str(",\"crate_types\":");
    push_string_array(&mut json, &record.classification.crate_types);
    json.push_str(",\"metadata\":");
    push_optional_string(&mut json, record.classification.metadata.as_deref());
    json.push_str(",\"extra_filename\":");
    push_optional_string(&mut json, record.classification.extra_filename.as_deref());
    json.push_str(",\"out_dir\":");
    push_optional_string(&mut json, record.classification.out_dir.as_deref());
    json.push_str(",\"injected\":");
    push_string_array(&mut json, record.injected);
    json.push_str(",\"pre_digest\":");
    push_json_string(&mut json, &framed_digest(record.arguments));
    json.push_str(",\"post_digest\":");
    push_json_string(&mut json, &framed_digest(record.final_arguments));
    json.push_str(",\"argv_hex\":");
    push_hex_array(&mut json, record.arguments);
    json.push_str(",\"argv_display\":");
    push_display_array(&mut json, record.arguments);
    json.push_str(",\"environment\":{");
    for (index, name) in OBSERVED_ENVIRONMENT.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(&mut json, name);
        json.push(':');
        match env::var_os(name) {
            Some(value) => {
                json.push_str("{\"present\":true,\"digest\":");
                push_json_string(&mut json, &hex(&sha256(value.as_bytes())));
                json.push_str(",\"empty\":");
                json.push_str(bool_literal(value.is_empty()));
                json.push('}');
            }
            None => json.push_str("{\"present\":false}"),
        }
    }
    json.push_str("}}\n");

    let identity = hex(&sha256(json.as_bytes()));
    for attempt in 0..64_u32 {
        let name = format!("{}-{:02}.json", &identity[..32], attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(&name))
        {
            Ok(mut file) => {
                if file.write_all(json.as_bytes()).is_err() {
                    protocol_failure("could not write the shim capture record");
                }
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => protocol_failure("could not create the shim capture record"),
        }
    }
    protocol_failure("exhausted the bounded capture-name retry policy");
}

fn bool_literal(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn push_optional_string(json: &mut String, value: Option<&str>) {
    match value {
        Some(value) => push_json_string(json, value),
        None => json.push_str("null"),
    }
}

fn push_string_array(json: &mut String, values: &[String]) {
    json.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(json, value);
    }
    json.push(']');
}

fn push_hex_array(json: &mut String, values: &[OsString]) {
    json.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(json, &hex(value.as_bytes()));
    }
    json.push(']');
}

fn push_display_array(json: &mut String, values: &[OsString]) {
    json.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(json, &value.to_string_lossy());
    }
    json.push(']');
}

fn push_json_string(json: &mut String, value: &str) {
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                json.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => json.push(character),
        }
    }
    json.push('"');
}

fn framed_digest(arguments: &[OsString]) -> String {
    let mut framed = Vec::new();
    framed.extend_from_slice(&(arguments.len() as u64).to_be_bytes());
    for argument in arguments {
        let bytes = argument.as_bytes();
        framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        framed.extend_from_slice(bytes);
    }
    hex(&sha256(&framed))
}

fn injection_mode() -> Injection {
    match env::var("TEMPER_EXP_INJECT") {
        Err(_) => Injection::None,
        Ok(value) if value.is_empty() || value == "none" => Injection::None,
        Ok(value) => {
            if let Some(path) = value.strip_prefix("generate=") {
                absolute_or_fail(path);
                Injection::Generate(path.to_owned())
            } else if let Some(path) = value.strip_prefix("use=") {
                absolute_or_fail(path);
                Injection::Use(path.to_owned())
            } else {
                protocol_failure("TEMPER_EXP_INJECT must be none, generate=<abs> or use=<abs>")
            }
        }
    }
}

fn absolute_or_fail(path: &str) {
    if !PathBuf::from(path).is_absolute() {
        protocol_failure("TEMPER_EXP_INJECT requires an absolute profile path");
    }
}

fn required_path(name: &str) -> PathBuf {
    match env::var_os(name) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => protocol_failure(&format!("missing private protocol value {name}")),
    }
}

fn required_string(name: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => protocol_failure(&format!("missing private protocol value {name}")),
    }
}

fn protocol_failure(message: &str) -> ! {
    eprintln!("temper shim protocol failure ({PROTOCOL}): {message}");
    std::process::exit(PROTOCOL_FAILURE_EXIT)
}

include!("sha256.rs");
