//! Cargo configuration provenance for schema 3.
//!
//! This module discovers the Cargo configuration source graph, follows the
//! stable `include` recursion Cargo 1.94 introduced, hashes every source, and
//! rejects a compiler override declared anywhere in that graph. It records
//! provenance only: it never merges configuration values, never reconstructs
//! effective rustflags and never becomes a second Cargo configuration engine
//! (FR-023, NFR-018).
//!
//! It also records the compiler-input environment as bounded digests, so a
//! changed `RUSTFLAGS` or wrapper variable stays auditable without publishing
//! its value (NFR-012).

use std::collections::HashSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Cargo minor release that stabilized the top-level `include` key.
pub(crate) const INCLUDE_STABLE_MINOR: u32 = 94;

const SOURCE_LIMIT: usize = 64;
const INCLUDE_DEPTH_LIMIT: usize = 16;
const SOURCE_BYTE_LIMIT: u64 = 1024 * 1024;
const DECLARED_LIMIT: usize = 256;

pub(crate) const VERSION_REASON: &str = "cargo_version_unrecognized";
pub(crate) const READ_REASON: &str = "cargo_config_read_failed";
pub(crate) const PARSE_REASON: &str = "cargo_config_parse_failed";
pub(crate) const MUTATED_REASON: &str = "cargo_config_source_mutated";
pub(crate) const LIMIT_REASON: &str = "cargo_config_source_limit";
pub(crate) const INCLUDE_MALFORMED_REASON: &str = "cargo_config_include_malformed";
pub(crate) const INCLUDE_MISSING_REASON: &str = "cargo_config_include_missing";
pub(crate) const INCLUDE_CYCLE_REASON: &str = "cargo_config_include_cycle";
pub(crate) const INCLUDE_UNSUPPORTED_REASON: &str = "cargo_config_include_unsupported_version";
pub(crate) const COMPILER_OVERRIDE_REASON: &str = "unproven_compiler_override";

/// Every configuration-source reason this scanner can produce. Reporting uses
/// it to classify a decision without matching diagnostic text.
pub(crate) const CONFIG_SOURCE_REASONS: [&str; 9] = [
    VERSION_REASON,
    READ_REASON,
    PARSE_REASON,
    MUTATED_REASON,
    LIMIT_REASON,
    INCLUDE_MALFORMED_REASON,
    INCLUDE_MISSING_REASON,
    INCLUDE_CYCLE_REASON,
    INCLUDE_UNSUPPORTED_REASON,
];

/// Compiler keys Temper cannot prove inactive without reimplementing Cargo's
/// scalar precedence, so any declaration rejects PGO.
const COMPILER_OVERRIDE_KEYS: [&str; 3] = ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"];

/// Compiler-input environment variables recorded as bounded digests.
const ENVIRONMENT_INPUTS: [&str; 12] = [
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_HOME",
];

/// How one configuration source entered the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Discovery {
    CargoHome,
    Ancestor,
    Include,
}

/// One `include` edge exactly as the including file declared it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct IncludeEdge {
    pub(crate) declared: String,
    pub(crate) resolved: PathBuf,
    pub(crate) optional: bool,
    pub(crate) present: bool,
}

/// One discovered Cargo configuration file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ConfigSource {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) discovery: Discovery,
    pub(crate) includes: Vec<IncludeEdge>,
}

/// Whether a compiler-input variable was absent, empty or set. Cargo treats an
/// empty wrapper as unset, so the two cases must stay distinguishable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Presence {
    Absent,
    Empty,
    Set,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct EnvironmentInput {
    pub(crate) name: &'static str,
    pub(crate) presence: Presence,
    pub(crate) sha256: Option<String>,
}

/// The complete configuration source graph of one PGO attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ConfigGraph {
    pub(crate) cargo_minor: u32,
    pub(crate) include_supported: bool,
    pub(crate) declares_include: bool,
    /// Sources in Cargo's documented load order: every included file precedes
    /// the file that includes it, and includes load from left to right.
    pub(crate) sources: Vec<ConfigSource>,
    pub(crate) environment_inputs: Vec<EnvironmentInput>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigGraphFailure {
    pub(crate) reason: &'static str,
    pub(crate) message: String,
    pub(crate) source: Option<PathBuf>,
}

impl ConfigGraphFailure {
    fn new(reason: &'static str, message: impl Into<String>, source: Option<&Path>) -> Self {
        Self {
            reason,
            message: message.into(),
            source: source.map(Path::to_path_buf),
        }
    }
}

/// Scans the configuration graph Cargo loads for a build anchored at
/// `workspace_root`, which is the working directory Temper gives Cargo.
pub(crate) fn scan(
    workspace_root: &Path,
    cargo_version: &str,
) -> Result<ConfigGraph, ConfigGraphFailure> {
    let cargo_minor = parse_minor(cargo_version)?;
    let mut scanner = Scanner {
        include_supported: cargo_minor >= INCLUDE_STABLE_MINOR,
        declares_include: false,
        sources: Vec::new(),
        visited: HashSet::new(),
        stack: Vec::new(),
    };
    for (discovery, path) in discover_roots(workspace_root) {
        scanner.load(&path, discovery)?;
    }
    Ok(ConfigGraph {
        cargo_minor,
        include_supported: scanner.include_supported,
        declares_include: scanner.declares_include,
        sources: scanner.sources,
        environment_inputs: environment_inputs(),
    })
}

struct Scanner {
    include_supported: bool,
    declares_include: bool,
    sources: Vec<ConfigSource>,
    visited: HashSet<PathBuf>,
    stack: Vec<PathBuf>,
}

impl Scanner {
    fn load(&mut self, path: &Path, discovery: Discovery) -> Result<(), ConfigGraphFailure> {
        let canonical = fs::canonicalize(path).map_err(|error| {
            ConfigGraphFailure::new(
                READ_REASON,
                format!(
                    "Cargo config {} could not be resolved: {error}",
                    path.display()
                ),
                Some(path),
            )
        })?;
        if self.stack.contains(&canonical) {
            return Err(ConfigGraphFailure::new(
                INCLUDE_CYCLE_REASON,
                format!(
                    "Cargo config {} takes part in an include cycle.",
                    canonical.display()
                ),
                Some(&canonical),
            ));
        }
        if self.visited.contains(&canonical) {
            return Ok(());
        }
        if self.stack.len() >= INCLUDE_DEPTH_LIMIT || self.sources.len() >= SOURCE_LIMIT {
            return Err(ConfigGraphFailure::new(
                LIMIT_REASON,
                format!(
                    "The Cargo config graph exceeded {SOURCE_LIMIT} sources or depth {INCLUDE_DEPTH_LIMIT}."
                ),
                Some(&canonical),
            ));
        }

        let (contents, sha256) = read_stable(&canonical)?;
        let config: toml::Value = toml::from_str(&contents).map_err(|error| {
            ConfigGraphFailure::new(
                PARSE_REASON,
                format!(
                    "Cargo config {} could not be parsed: {error}",
                    canonical.display()
                ),
                Some(&canonical),
            )
        })?;
        reject_compiler_override(&config, &canonical)?;
        let edges = include_edges(&config, &canonical)?;
        if !edges.is_empty() {
            self.declares_include = true;
            if !self.include_supported {
                return Err(ConfigGraphFailure::new(
                    INCLUDE_UNSUPPORTED_REASON,
                    format!(
                        "Cargo config {} uses the stable include key, which requires Cargo 1.{INCLUDE_STABLE_MINOR} or newer.",
                        canonical.display()
                    ),
                    Some(&canonical),
                ));
            }
        }

        // Included files load first and are overridden by the including file,
        // so they precede it in the recorded order.
        self.stack.push(canonical.clone());
        let included = self.load_includes(&edges, &canonical);
        self.stack.pop();
        included?;

        self.visited.insert(canonical.clone());
        self.sources.push(ConfigSource {
            path: canonical,
            sha256,
            discovery,
            includes: edges,
        });
        Ok(())
    }

    fn load_includes(
        &mut self,
        edges: &[IncludeEdge],
        including: &Path,
    ) -> Result<(), ConfigGraphFailure> {
        for edge in edges {
            if edge.present {
                self.load(&edge.resolved, Discovery::Include)?;
            } else if !edge.optional {
                return Err(ConfigGraphFailure::new(
                    INCLUDE_MISSING_REASON,
                    format!(
                        "Cargo config {} requires include {} which does not exist.",
                        including.display(),
                        edge.resolved.display()
                    ),
                    Some(including),
                ));
            }
        }
        Ok(())
    }
}

/// Reads one source twice so a file changed during discovery cannot be hashed
/// as stable evidence.
fn read_stable(path: &Path) -> Result<(String, String), ConfigGraphFailure> {
    let first = read_bounded(path)?;
    let second = read_bounded(path)?;
    stable_source(path, first, &second)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ConfigGraphFailure> {
    let metadata = fs::metadata(path).map_err(|error| {
        ConfigGraphFailure::new(
            READ_REASON,
            format!(
                "Cargo config {} could not be inspected: {error}",
                path.display()
            ),
            Some(path),
        )
    })?;
    if !metadata.is_file() {
        return Err(ConfigGraphFailure::new(
            READ_REASON,
            format!("Cargo config {} is not a regular file.", path.display()),
            Some(path),
        ));
    }
    if metadata.len() > SOURCE_BYTE_LIMIT {
        return Err(ConfigGraphFailure::new(
            LIMIT_REASON,
            format!("Cargo config {} exceeded 1 MiB.", path.display()),
            Some(path),
        ));
    }
    fs::read(path).map_err(|error| {
        ConfigGraphFailure::new(
            READ_REASON,
            format!("Cargo config {} could not be read: {error}", path.display()),
            Some(path),
        )
    })
}

fn stable_source(
    path: &Path,
    first: Vec<u8>,
    second: &[u8],
) -> Result<(String, String), ConfigGraphFailure> {
    let sha256 = digest(&first);
    if digest(second) != sha256 {
        return Err(ConfigGraphFailure::new(
            MUTATED_REASON,
            format!(
                "Cargo config {} changed while it was hashed.",
                path.display()
            ),
            Some(path),
        ));
    }
    let contents = String::from_utf8(first).map_err(|_| {
        ConfigGraphFailure::new(
            READ_REASON,
            format!("Cargo config {} is not valid UTF-8.", path.display()),
            Some(path),
        )
    })?;
    Ok((contents, sha256))
}

fn reject_compiler_override(config: &toml::Value, path: &Path) -> Result<(), ConfigGraphFailure> {
    let Some(build) = config.get("build") else {
        return Ok(());
    };
    for key in COMPILER_OVERRIDE_KEYS {
        if build.get(key).is_some() {
            return Err(ConfigGraphFailure::new(
                COMPILER_OVERRIDE_REASON,
                format!(
                    "PGO was rejected because effective build.{key} may come from {}.",
                    path.display()
                ),
                Some(path),
            ));
        }
    }
    Ok(())
}

/// Parses Cargo's documented `include` shapes: a list of strings, or a list of
/// tables carrying `path` and an optional `optional` flag. Every other shape is
/// unsupported rather than guessed.
fn include_edges(
    config: &toml::Value,
    path: &Path,
) -> Result<Vec<IncludeEdge>, ConfigGraphFailure> {
    let Some(value) = config.get("include") else {
        return Ok(Vec::new());
    };
    let malformed = |detail: &str| {
        ConfigGraphFailure::new(
            INCLUDE_MALFORMED_REASON,
            format!("Cargo config {} declares {detail}.", path.display()),
            Some(path),
        )
    };
    let entries = value
        .as_array()
        .ok_or_else(|| malformed("an include value that is not a list"))?;
    if entries.len() > DECLARED_LIMIT {
        return Err(ConfigGraphFailure::new(
            LIMIT_REASON,
            format!(
                "Cargo config {} declares more than {DECLARED_LIMIT} includes.",
                path.display()
            ),
            Some(path),
        ));
    }
    let directory = path
        .parent()
        .ok_or_else(|| malformed("an include in a file with no parent directory"))?;
    let mut edges = Vec::with_capacity(entries.len());
    for entry in entries {
        let (declared, optional) = match entry {
            toml::Value::String(declared) => (declared.as_str(), false),
            toml::Value::Table(table) => {
                if table.keys().any(|key| key != "path" && key != "optional") {
                    return Err(malformed("an include table with an unsupported key"));
                }
                let declared = table
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| malformed("an include table without a string path"))?;
                let optional = match table.get("optional") {
                    None => false,
                    Some(toml::Value::Boolean(optional)) => *optional,
                    Some(_) => {
                        return Err(malformed("an include optional flag that is not a boolean"));
                    }
                };
                (declared, optional)
            }
            _ => {
                return Err(malformed(
                    "an include entry that is neither a string nor a table",
                ));
            }
        };
        if !declared.ends_with(".toml") {
            return Err(malformed("an include path that does not end in .toml"));
        }
        if declared.contains(['*', '?', '[', ']', '{', '}']) {
            return Err(malformed("an include path containing a pattern character"));
        }
        let resolved = directory.join(declared);
        edges.push(IncludeEdge {
            declared: declared.to_owned(),
            present: resolved.is_file(),
            resolved,
            optional,
        });
    }
    Ok(edges)
}

/// Cargo discovers configuration from `CARGO_HOME` and from the ancestors of
/// its working directory, which Temper anchors at the workspace root.
fn discover_roots(workspace_root: &Path) -> Vec<(Discovery, PathBuf)> {
    let mut roots = Vec::new();
    if let Some(cargo_home) = cargo_home(workspace_root)
        && let Some(path) = active_config_path(&cargo_home)
    {
        roots.push((Discovery::CargoHome, path));
    }
    let mut ancestors: Vec<&Path> = workspace_root.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if let Some(path) = active_config_path(&ancestor.join(".cargo"))
            && !roots.iter().any(|(_, known)| *known == path)
        {
            roots.push((Discovery::Ancestor, path));
        }
    }
    roots
}

fn cargo_home(workspace_root: &Path) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("CARGO_HOME") {
        let configured = PathBuf::from(configured);
        return Some(if configured.is_absolute() {
            configured
        } else {
            workspace_root.join(configured)
        });
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".cargo"))
}

fn active_config_path(directory: &Path) -> Option<PathBuf> {
    let legacy = directory.join("config");
    if legacy.is_file() {
        Some(legacy)
    } else {
        let toml = directory.join("config.toml");
        toml.is_file().then_some(toml)
    }
}

fn environment_inputs() -> Vec<EnvironmentInput> {
    ENVIRONMENT_INPUTS
        .into_iter()
        .map(|name| match std::env::var_os(name) {
            None => EnvironmentInput {
                name,
                presence: Presence::Absent,
                sha256: None,
            },
            Some(value) if value.is_empty() => EnvironmentInput {
                name,
                presence: Presence::Empty,
                sha256: None,
            },
            Some(value) => EnvironmentInput {
                name,
                presence: Presence::Set,
                sha256: Some(digest(value.as_os_str().as_bytes())),
            },
        })
        .collect()
}

/// Parses the minor release from `cargo -Vv` output such as
/// `cargo 1.97.1 (c980f4866 2026-06-30)`.
fn parse_minor(cargo_version: &str) -> Result<u32, ConfigGraphFailure> {
    let unrecognized = || {
        ConfigGraphFailure::new(
            VERSION_REASON,
            "The active Cargo did not report a recognizable version.",
            None,
        )
    };
    let version = cargo_version
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("cargo "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(unrecognized)?;
    let minor = version.split('.').nth(1).ok_or_else(unrecognized)?;
    let digits: String = minor.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().map_err(|_| unrecognized())
}

fn digest(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{
        COMPILER_OVERRIDE_REASON, ConfigGraph, ConfigSource, Discovery, INCLUDE_CYCLE_REASON,
        INCLUDE_MALFORMED_REASON, INCLUDE_MISSING_REASON, INCLUDE_UNSUPPORTED_REASON,
        MUTATED_REASON, Presence, parse_minor, scan, stable_source,
    };
    use std::fs;
    use std::path::Path;

    const CARGO_194: &str = "cargo 1.97.1 (c980f4866 2026-06-30)\nrelease: 1.97.1";
    const CARGO_193: &str = "cargo 1.93.1 (083ac5135 2025-12-15)\nrelease: 1.93.1";

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("config fixture");
        let cargo = root.path().join(".cargo");
        fs::create_dir(&cargo).expect("cargo directory");
        for (name, contents) in files {
            fs::write(cargo.join(name), contents).expect("config file");
        }
        root
    }

    fn graph(root: &Path, version: &str) -> ConfigGraph {
        scan(root, version).expect("config graph")
    }

    /// Only the fixture's own sources. An ambient `CARGO_HOME` configuration is
    /// legitimately discovered but is not what these cases assert about.
    fn fixture_sources<'a>(graph: &'a ConfigGraph, root: &Path) -> Vec<&'a ConfigSource> {
        let root = fs::canonicalize(root).expect("canonical fixture root");
        graph
            .sources
            .iter()
            .filter(|source| source.path.starts_with(&root))
            .collect()
    }

    #[test]
    fn includes_load_left_to_right_before_the_including_file() {
        let fixture = fixture(&[
            (
                "config.toml",
                "include = [\"first.toml\", \"second.toml\"]\n",
            ),
            ("first.toml", "include = [\"nested.toml\"]\n"),
            (
                "nested.toml",
                "[build]\nrustflags = [\"--cfg\", \"nested\"]\n",
            ),
            ("second.toml", "[build]\njobs = 1\n"),
        ]);
        let graph = graph(fixture.path(), CARGO_194);
        let sources = fixture_sources(&graph, fixture.path());
        let names: Vec<String> = sources
            .iter()
            .filter_map(|source| source.path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["nested.toml", "first.toml", "second.toml", "config.toml"]
        );
        assert!(graph.declares_include);
        assert!(sources.iter().all(|source| source.sha256.len() == 64));

        let including = sources.last().expect("including file");
        assert_eq!(including.discovery, Discovery::Ancestor);
        assert_eq!(including.includes.len(), 2);
        assert!(including.includes.iter().all(|edge| edge.present));
        // Every include path resolves against the including file's directory.
        assert_eq!(
            including.includes[0].resolved,
            fixture.path().join(".cargo/first.toml")
        );
        assert_eq!(sources[1].discovery, Discovery::Include);
    }

    #[test]
    fn a_missing_optional_include_is_recorded_without_failure() {
        let fixture = fixture(&[(
            "config.toml",
            "include = [{ path = \"absent.toml\", optional = true }]\n",
        )]);
        let graph = graph(fixture.path(), CARGO_194);
        let sources = fixture_sources(&graph, fixture.path());
        let edge = &sources.last().expect("config").includes[0];
        assert!(edge.optional);
        assert!(!edge.present);
        assert_eq!(edge.declared, "absent.toml");
    }

    #[test]
    fn every_unsupported_include_shape_fails_closed() {
        let cases: [(&str, &str, &str); 5] = [
            (
                "missing required",
                "include = [\"absent.toml\"]\n",
                INCLUDE_MISSING_REASON,
            ),
            (
                "bare string",
                "include = \"single.toml\"\n",
                INCLUDE_MALFORMED_REASON,
            ),
            (
                "non-toml path",
                "include = [\"flags.txt\"]\n",
                INCLUDE_MALFORMED_REASON,
            ),
            (
                "pattern path",
                "include = [\"flags-*.toml\"]\n",
                INCLUDE_MALFORMED_REASON,
            ),
            (
                "unsupported table key",
                "include = [{ path = \"a.toml\", required = true }]\n",
                INCLUDE_MALFORMED_REASON,
            ),
        ];
        for (label, contents, expected) in cases {
            let fixture = fixture(&[("config.toml", contents)]);
            let failure = scan(fixture.path(), CARGO_194).expect_err(label);
            assert_eq!(failure.reason, expected, "{label}");
            assert!(failure.source.is_some(), "{label} names no source");
        }
    }

    #[test]
    fn an_include_cycle_is_rejected_instead_of_partially_accepted() {
        let fixture = fixture(&[
            ("config.toml", "include = [\"a.toml\"]\n"),
            ("a.toml", "include = [\"b.toml\"]\n"),
            ("b.toml", "include = [\"a.toml\"]\n"),
        ]);
        let failure = scan(fixture.path(), CARGO_194).expect_err("cycle");
        assert_eq!(failure.reason, INCLUDE_CYCLE_REASON);
    }

    #[test]
    fn stable_include_below_cargo_194_is_a_version_boundary() {
        let including = fixture(&[
            ("config.toml", "include = [\"flags.toml\"]\n"),
            ("flags.toml", "[build]\njobs = 1\n"),
        ]);
        let failure = scan(including.path(), CARGO_193).expect_err("version boundary");
        assert_eq!(failure.reason, INCLUDE_UNSUPPORTED_REASON);

        // A configuration without include keeps the prior supported boundary.
        let plain = fixture(&[("config.toml", "[build]\njobs = 1\n")]);
        let graph = graph(plain.path(), CARGO_193);
        assert!(!graph.include_supported);
        assert!(!graph.declares_include);
        assert_eq!(graph.cargo_minor, 93);
    }

    #[test]
    fn a_compiler_override_is_detected_through_the_include_graph() {
        for key in ["rustc", "rustc-wrapper", "rustc-workspace-wrapper"] {
            let fixture = fixture(&[
                ("config.toml", "include = [\"compiler.toml\"]\n"),
                (
                    "compiler.toml",
                    &format!("[build]\n{key} = \"/other/compiler\"\n"),
                ),
            ]);
            let failure = scan(fixture.path(), CARGO_194).expect_err(key);
            assert_eq!(failure.reason, COMPILER_OVERRIDE_REASON);
            assert!(failure.message.contains(&format!("build.{key}")));
            assert!(
                failure
                    .source
                    .as_ref()
                    .is_some_and(|source| source.ends_with("compiler.toml")),
                "{key} did not name the included source"
            );
        }
    }

    #[test]
    fn a_source_that_changes_between_reads_is_not_hashed_as_stable() {
        let path = Path::new("/workspace/.cargo/config.toml");
        let (contents, sha256) = stable_source(
            path,
            b"[build]\njobs = 1\n".to_vec(),
            b"[build]\njobs = 1\n",
        )
        .expect("stable source");
        assert_eq!(contents, "[build]\njobs = 1\n");
        assert_eq!(sha256.len(), 64);

        let failure = stable_source(
            path,
            b"[build]\njobs = 1\n".to_vec(),
            b"[build]\njobs = 2\n",
        )
        .expect_err("mutated source");
        assert_eq!(failure.reason, MUTATED_REASON);
        assert_eq!(failure.source.as_deref(), Some(path));
    }

    #[test]
    fn environment_inputs_distinguish_absence_from_an_empty_value() {
        let fixture = fixture(&[("config.toml", "[build]\njobs = 1\n")]);
        let graph = graph(fixture.path(), CARGO_194);
        let inputs = &graph.environment_inputs;
        assert_eq!(inputs.len(), 12);
        assert!(inputs.iter().any(|input| input.name == "RUSTFLAGS"));
        assert!(
            inputs
                .iter()
                .any(|input| input.name == "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS")
        );
        for input in inputs {
            match input.presence {
                Presence::Set => assert_eq!(input.sha256.as_deref().map(str::len), Some(64)),
                Presence::Absent | Presence::Empty => assert!(input.sha256.is_none()),
            }
        }
    }

    #[test]
    fn cargo_minor_versions_parse_or_fail_closed() {
        assert_eq!(parse_minor(CARGO_194).expect("stable"), 97);
        assert_eq!(
            parse_minor("cargo 1.94.0-nightly (abc 2026-01-01)").expect("nightly"),
            94
        );
        assert!(parse_minor("rustc 1.97.1").is_err());
        assert!(parse_minor("cargo 1").is_err());
    }
}
