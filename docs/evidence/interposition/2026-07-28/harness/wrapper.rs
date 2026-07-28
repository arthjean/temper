// Temper v0.0.3 EP-001 experiment: a compiler wrapper used to observe Cargo's
// documented `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC` chain and to
// emulate an outer compiler cache.
//
//   TEMPER_EXP_WRAPPER_MODE       `log` (default) | `cache`
//   TEMPER_EXP_WRAPPER_LABEL      label recorded in every log record
//   TEMPER_EXP_WRAPPER_LOG        append-only JSONL log path (required)
//   TEMPER_EXP_WRAPPER_STRIP      path replaced by `<ROOT>` before key hashing
//   TEMPER_EXP_WRAPPER_CACHE_DIR  cache root used by `cache` mode
//
// `cache` mode is a documented emulation of a build-directory independent
// compiler cache such as sccache: output and dependency paths are normalized
// out of the cache key, exactly as a cache must do to hit across build
// directories. It is not sccache and makes no claim about sccache internals.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let Some((program, forwarded)) = arguments.split_first() else {
        eprintln!("wrapper: no wrapped program was supplied");
        std::process::exit(96);
    };
    let log = env::var_os("TEMPER_EXP_WRAPPER_LOG").expect("TEMPER_EXP_WRAPPER_LOG is required");
    let label = env::var("TEMPER_EXP_WRAPPER_LABEL").unwrap_or_else(|_| {
        env::args_os()
            .next()
            .map(PathBuf::from)
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "wrapper".to_owned())
    });
    let cache = env::var("TEMPER_EXP_WRAPPER_MODE").as_deref() == Ok("cache");

    let strip = env::var_os("TEMPER_EXP_WRAPPER_STRIP").map(PathBuf::from);
    let key = cache_key(program, forwarded, strip.as_deref());
    let crate_name = value_of(forwarded, "--crate-name");
    let out_dir = value_of(forwarded, "--out-dir");
    let compiles = forwarded
        .iter()
        .any(|argument| argument.as_bytes().starts_with(b"--emit"));

    let cacheable = env::var("TEMPER_EXP_WRAPPER_CACHE_CRATE")
        .ok()
        .filter(|value| !value.is_empty())
        .is_none_or(|wanted| crate_name.as_deref() == Some(wanted.as_str()));

    if !cache || !compiles || !cacheable {
        append_log(
            &log,
            &label,
            &key,
            "pass_through",
            crate_name.as_deref(),
            program,
            forwarded,
        );
        let error = Command::new(program).args(forwarded).exec();
        eprintln!("wrapper: could not execute the wrapped program: {error}");
        std::process::exit(96);
    }

    let cache_root = PathBuf::from(
        env::var_os("TEMPER_EXP_WRAPPER_CACHE_DIR")
            .expect("TEMPER_EXP_WRAPPER_CACHE_DIR is required in cache mode"),
    );
    let entry = cache_root.join(&key);
    let out_dir = out_dir
        .map(PathBuf::from)
        .expect("a compile invocation carries --out-dir");

    if entry.is_dir() {
        for file in fs::read_dir(&entry).expect("read cache entry") {
            let file = file.expect("cache entry file");
            fs::copy(file.path(), out_dir.join(file.file_name())).expect("replay cached artifact");
        }
        append_log(
            &log,
            &label,
            &key,
            "cache_hit",
            crate_name.as_deref(),
            program,
            forwarded,
        );
        return;
    }

    let before = snapshot(&out_dir);
    let status = Command::new(program)
        .args(forwarded)
        .status()
        .expect("run the wrapped compiler");
    if status.success() {
        fs::create_dir_all(&entry).expect("create cache entry");
        for (name, stamp) in snapshot(&out_dir) {
            if before.get(&name) != Some(&stamp) {
                let source = out_dir.join(&name);
                if source.is_file() {
                    fs::copy(&source, entry.join(&name)).expect("store cached artifact");
                }
            }
        }
    }
    append_log(
        &log,
        &label,
        &key,
        "cache_miss",
        crate_name.as_deref(),
        program,
        forwarded,
    );
    std::process::exit(status.code().unwrap_or(1));
}

fn snapshot(directory: &Path) -> std::collections::BTreeMap<OsString, (u64, u128)> {
    let mut entries = std::collections::BTreeMap::new();
    let Ok(listing) = fs::read_dir(directory) else {
        return entries;
    };
    for entry in listing.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        entries.insert(entry.file_name(), (metadata.len(), modified));
    }
    entries
}

fn cache_key(program: &OsStr, forwarded: &[OsString], strip: Option<&Path>) -> String {
    let mut framed = Vec::new();
    frame(&mut framed, program.as_bytes());
    for argument in forwarded {
        let normalized = normalize(argument, strip);
        if normalized.is_empty() {
            continue;
        }
        frame(&mut framed, &normalized);
    }
    for name in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"] {
        match env::var_os(name) {
            Some(value) => frame(&mut framed, value.as_os_str().as_bytes()),
            None => frame(&mut framed, b""),
        }
    }
    hex(&sha256(&framed))
}

fn normalize(argument: &OsString, strip: Option<&Path>) -> Vec<u8> {
    let bytes = argument.as_os_str().as_bytes().to_vec();
    let Some(strip) = strip else {
        return bytes;
    };
    let needle = strip.as_os_str().as_bytes();
    if needle.is_empty() {
        return bytes;
    }
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(needle) {
            output.extend_from_slice(b"<ROOT>");
            index += needle.len();
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    output
}

fn frame(buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    buffer.extend_from_slice(bytes);
}

fn value_of(arguments: &[OsString], name: &str) -> Option<String> {
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index].as_os_str() == OsStr::new(name) {
            return arguments
                .get(index + 1)
                .map(|value| value.to_string_lossy().into_owned());
        }
        index += 1;
    }
    None
}

fn append_log(
    path: &OsString,
    label: &str,
    key: &str,
    outcome: &str,
    crate_name: Option<&str>,
    program: &OsStr,
    forwarded: &[OsString],
) {
    let mut framed = Vec::new();
    frame(&mut framed, program.as_bytes());
    for argument in forwarded {
        frame(&mut framed, argument.as_os_str().as_bytes());
    }
    let mut json = String::new();
    json.push_str("{\"label\":");
    push_json_string(&mut json, label);
    json.push_str(",\"outcome\":");
    push_json_string(&mut json, outcome);
    json.push_str(",\"cache_key\":");
    push_json_string(&mut json, key);
    json.push_str(",\"received_digest\":");
    push_json_string(&mut json, &hex(&sha256(&framed)));
    json.push_str(",\"crate_name\":");
    match crate_name {
        Some(value) => push_json_string(&mut json, value),
        None => json.push_str("null"),
    }
    json.push_str(",\"next_program\":");
    push_json_string(&mut json, &program.to_string_lossy());
    json.push_str(",\"received_argv\":[");
    for (index, argument) in forwarded.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        push_json_string(&mut json, &argument.to_string_lossy());
    }
    json.push_str("]}\n");

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))
        .expect("open wrapper log");
    file.write_all(json.as_bytes()).expect("append wrapper log");
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

include!("sha256.rs");
