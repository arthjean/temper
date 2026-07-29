# PGO compatibility matrix v1

The deterministic integration matrix is split into independently reported Rust
tests so one failed boundary stays named. `cargo test --all-targets --locked`
executes every non-ignored case.

Matrix A covers Cargo configuration provenance (v0.0.3 US-011). Matrix B covers
the wrapper, process and evidence boundary (v0.0.3 US-012). The remaining table
retains the v0.0.2 PGO cases that are still part of the boundary.

## Tested toolchain

| Field | Value |
|---|---|
| Cargo | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| rustc | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6 |
| Host | Linux 7.1.5-200.fc44.x86_64, `x86_64-unknown-linux-gnu` |
| Temper | v0.0.3, report schema 3, shim protocol `temper-rustc-shim-1` |

A failure on a newer Cargo blocks every compatibility claim in this document
until the change is classified.

## Matrix A: Cargo rustflags and include provenance

Every supported case guards its own source with `compile_error!`, so a lost
compiler input is an executed build failure rather than an inspected field, and
the resolved argument order is checked against independently recomputed
length-framed digests in every interposed stage.

| Case | Expected result | Executable proof (`tests/ep004_config_matrix.rs`) |
|---|---|---|
| no rustflags | Full PGO, no declared include | `absent_rustflags_complete_pgo_with_no_declared_include` |
| direct string and array target rustflags | Sentinel survives every phase in order | `direct_string_and_array_target_rustflags_survive_every_phase` |
| required include, nested include | Sentinel survives; sources recorded deepest first | `required_and_nested_includes_survive_every_phase` |
| missing optional include | Absent edge recorded, required flag survives | `a_missing_optional_include_is_recorded_and_the_required_flag_survives` |
| target cfg table, `build.rustflags` | Sentinel survives every phase in order | `target_cfg_and_build_rustflags_survive_every_phase` |
| `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` | Sentinel survives every phase in order | `ambient_and_encoded_rustflags_survive_every_phase` |
| precedence: target over `build`, `RUSTFLAGS` over included target, encoded over `RUSTFLAGS` | Only the winning source reaches rustc, in its resolved order | `cargo_precedence_selects_one_source_and_preserves_its_order` |
| build script and proc macro alongside a target dependency | Zero phase controls on host units, exactly one on each target compilation | `build_scripts_and_proc_macros_receive_no_phase_control` |
| include table key Cargo accepts and Temper cannot prove | Reject only PGO as `cargo_config_include_malformed` | `an_include_shape_cargo_accepts_but_temper_cannot_prove_rejects_only_pgo` |
| include cycle, missing required include, non-list include | Cargo rejects the configuration; the run fails before screening and publishes no pointer | `cargo_rejected_include_graphs_fail_before_any_screening` |
| included config changed between PGO phases | Reject only PGO as `pgo_phase_parity_mismatch` with the `config_graph` difference class, before the optimized build | `an_included_config_change_between_phases_rejects_before_screening` |
| direct, split-form and included profiling or coverage control | Reject only PGO as `pgo_compiler_input_conflict` before rustc runs | `direct_split_form_and_included_profiling_controls_reject_only_pgo` |
| included rustflag through confirmation | Confirmation build is interposed and its parity matches | `an_included_rustflag_survives_through_confirmation` |

An include cycle, a missing required include and a non-list `include` value are
rejected by Cargo's own configuration loader before Temper can classify them, so
the matrix asserts the observable contract: the run fails, publishes no success
pointer and promotes nothing. Temper's own `cargo_config_include_cycle`,
`cargo_config_include_missing`, `cargo_config_include_unsupported_version` and
`cargo_config_source_mutated` reasons are exercised directly by the
`src/config_graph.rs` unit tests. `-Cprofile-use` as a pre-existing conflicting
control is covered by `rejects_existing_profiling_and_coverage_controls` in
`src/interposition.rs`, because a syntactically valid but nonexistent profile
would fail the uninterposed baseline build first.

## Matrix B: wrapper rejection, process and evidence

| Case | Expected result | Executable proof |
|---|---|---|
| no wrapper anywhere | Reference, generate, use and confirmation complete with unique roots and complete target captures | `the_no_wrapper_control_completes_every_interposed_stage` (`ep004_wrapper_matrix.rs`) |
| ambient general, workspace, nested and `CARGO_BUILD_RUSTC_WRAPPER` | Reject only PGO as `ambient_compiler_override` before the reference build, with no capture directory | `ambient_general_workspace_and_nested_wrappers_reject_only_pgo` |
| `build.rustc-wrapper`, `build.rustc-workspace-wrapper`, `build.rustc`, direct and through two include levels | Reject only PGO as `unproven_compiler_override`, naming the declaring source | `configured_wrappers_and_compilers_reject_only_pgo_through_nested_includes` |
| rejected wrapper value | Named without publishing its unrestricted value in `run.json` or the report | `a_rejected_wrapper_is_named_without_publishing_its_value` |
| pre-existing or symlinked stage target directory | Reject as `pgo_stage_isolation_failed` before Cargo starts | `a_pre_existing_or_symlinked_stage_directory_rejects_before_cargo_starts` |
| capture record past the bounded record size | Reject only PGO as `compiler_capture_limit` | `an_over_budget_capture_record_rejects_only_pgo` |
| paired cold builds, release shim | Median overhead at most 5% and identical pass-through SHA-256 | `paired_pass_through_builds_keep_their_identity_and_overhead_budget` (ignored; run through `scripts/collect-v0.0.3-evidence.sh`) |
| public CLI surface | No shim subcommand or private variable in any help output | `the_public_cli_exposes_no_shim_surface` (`ep002_interposition.rs`) |
| shim dispatch before parsing | Ordinary CLI argv is opaque in shim mode | `shim_dispatch_precedes_command_line_parsing` |
| success, nonzero exit, signal, stdin, stdout, stderr | Process semantics preserved through `exec` | `process_replacement_preserves_streams_status_and_signals` |
| non-UTF-8 argument bytes | Forwarded byte for byte | `argument_bytes_survive_process_replacement_without_conversion` |
| eight private-protocol defects, duplicated protocol value | Exit 97, one bounded diagnostic, no fallback compiler | `protocol_failures_fail_closed_without_any_fallback_compiler`, `a_duplicated_private_value_fails_closed` |
| probe, host build script, host path containing the triple, version probe | Pass through with zero phase controls | `phase_controls_reach_target_compilations_only` |
| ambiguous target or identity shape | Reject as `pgo_compiler_input_ambiguous` | `an_ambiguous_argument_shape_rejects_instead_of_guessing` |
| 100 concurrent shim processes | No record overwritten, no identity collision | `concurrent_shim_processes_never_overwrite_a_record` |
| truncated, symlinked and duplicate-identity captures | Reject only PGO as `compiler_capture_corrupt` | `tampered_capture_evidence_rejects_only_pgo` |
| capture argv mutated after a valid artifact | Reject only PGO as `compiler_input_mismatch` with a named crate | `an_altered_use_stage_capture_rejects_only_pgo_with_a_compiler_input_mismatch` (`ep003_schema.rs`) |
| 10 adversarial parity mutations by difference class | Each rejects with its bounded class; record order alone is not a difference | `every_adversarial_mutation_rejects_with_its_bounded_class` (`src/parity.rs`) |
| stage directory reuse and canonical aliasing | Rejected at claim time | `stage_directories_must_be_unique_and_initially_absent` (`src/interposition.rs`) |
| more records than the stage budget | Reject as `compiler_capture_limit` | `more_records_than_the_stage_budget_fail_closed` (`src/interposition.rs`) |
| interrupted run | Run marked interrupted, workload tree removed, no pointer published | `sigint_marks_the_run_interrupted_and_removes_the_workload_tree` (`ep002_workload.rs`) |

## Retained v0.0.2 PGO cases

| Case | Expected result | Executable proof |
|---|---|---|
| absent rustflags, baseline base | Full PGO, matched parity | `supported_absent_rustflags_and_baseline_base_complete_pgo` |
| string rustflags, ThinLTO base, workspace, build script, proc macro, target dependency | Full PGO, host tools uninstrumented | `supported_string_rustflags_workspace_and_host_tools_complete_pgo` |
| array rustflags with embedded space, FatLTO base, spaced path | Full PGO, argument boundaries preserved | `supported_array_rustflags_and_space_paths_complete_pgo` |
| parity mismatch | Reject before optimized build | `changed_config_hash_rejects_only_pgo_before_optimization` |
| JSON-looking parser fallback | Reject only PGO as `unrecognized_cargo_json` | `pgo_json_looking_stream_rejects_only_pgo` |
| zero matching artifacts | Reject only PGO as `compiler_capture_missing` | `pgo_zero_executable_stream_rejects_only_pgo` |
| multiple matching artifacts | Reject only PGO as `cargo_executable_ambiguous` | `pgo_multiple_executable_stream_rejects_only_pgo` |
| zero profiles | Reject before merge | `builds_screens_and_records_the_fixed_strategy_order` |
| corrupt profile | Strict merge rejection | `corrupt_profile_rejects_only_pgo_during_strict_merge` |
| merge diagnostics with exit zero | Merge rejection | `merge_diagnostics_reject_a_zero_exit_merge` |
| LLVM missing-function warning | `pgo_missing_profile_data`, zero PGO samples | `optimized_missing_profile_diagnostic_rejects_only_pgo` |
| symlinked raw profile | Reject before merge | `symlinked_raw_profile_rejects_before_merge` |
| non-regular raw profile | Reject before merge | `non_regular_raw_profile_rejects_before_merge` |
| excessive raw profiles | Reject above 10,000 | `excessive_raw_profiles_reject_before_merge` |
| symlinked merged output | Reject before `llvm-profdata` | `preexisting_merged_profile_symlink_rejects_before_profdata_starts` |

v0.0.2's `ambient_rustflags_reject_only_pgo` case no longer exists: v0.0.3 stops
synthesizing `CARGO_ENCODED_RUSTFLAGS`, so ambient rustflags are a supported
input rather than a rejection. They are covered by Matrix A instead.

Every PGO-boundary rejection persists one stable reason, gives PGO no screening
sample, and leaves built static strategies eligible. The EP-002 matrix helpers
compare tracked source, manifest and lockfile checksums before and after each
new supported and profile-boundary execution.

## Known predicate limitation

The matrices above use fixtures Temper controls. Running one real corpus-v1 case
(`hexyl`) on 2026-07-29 exposed a shape none of them contained: the `rustix`
build script probes the compiler for the requested target with
`rustc --crate-type=rlib --emit=metadata --target <triple> --out-dir <build out> -`
and no `--crate-name`. That is an exact-target compilation `--emit`, so the
predicate cannot separate it from a real unit and rejects the whole PGO attempt
with `pgo_compiler_input_ambiguous`. Static candidates stayed eligible and the
run completed normally, but PGO is unavailable for any dependency tree
containing such a probe, which includes much of the common CLI ecosystem.

The behaviour is what the PRD specifies: fail closed, never broaden the
predicate without evidence. The shape is pinned by
`rejects_ambiguous_target_and_identity_shapes` in `src/interposition.rs` so any
future widening is a deliberate, evidenced change rather than a silent one.

## Overhead fixture

The paired cold-build benchmark builds `tests/fixtures/pgo-workspace` (a build
script, a proc macro, a target dependency and the selected binary) into a fresh
target directory for each arm, alternating which arm runs first and discarding
one warm-up pair. The interposed arm uses a **release** `cargo-temper` because
that is what users install. The fixture is small, so per-`exec` cost is amortized
over few compiler invocations; the result does not transfer to a large
workspace.
