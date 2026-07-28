# PGO compatibility matrix v1

The deterministic integration matrix is split into independently reported Rust
tests so one failed boundary remains named. `cargo test --all-targets --locked`
executes every non-ignored case.

| Case | Expected result | Executable proof |
|---|---|---|
| absent rustflags, baseline base | Full PGO, matched parity | `supported_absent_rustflags_and_baseline_base_complete_pgo` |
| string rustflags, ThinLTO base, workspace, build script, proc macro | Full PGO, host tools uninstrumented | `supported_string_rustflags_workspace_and_host_tools_complete_pgo` |
| array rustflags with embedded space, FatLTO base, spaced path | Full PGO, argument boundaries preserved | `supported_array_rustflags_and_space_paths_complete_pgo` |
| ambient rustflags | Reject only PGO | `ambient_rustflags_reject_only_pgo` |
| parity mismatch | Reject before optimized build | `changed_config_hash_rejects_only_pgo_before_optimization` |
| JSON-looking parser fallback | Reject only PGO as `unrecognized_cargo_json` | `pgo_json_looking_stream_rejects_only_pgo` |
| zero matching artifacts | Reject only PGO as `cargo_executable_missing` | `pgo_zero_executable_stream_rejects_only_pgo` |
| multiple matching artifacts | Reject only PGO as `cargo_executable_ambiguous` | `pgo_multiple_executable_stream_rejects_only_pgo` |
| zero profiles | Reject before merge | `builds_screens_and_records_the_fixed_strategy_order` |
| corrupt profile | Strict merge rejection | `corrupt_profile_rejects_only_pgo_during_strict_merge` |
| merge diagnostics with exit zero | Merge rejection | `merge_diagnostics_reject_a_zero_exit_merge` |
| LLVM missing-function warning | `pgo_missing_profile_data`, zero PGO samples | `optimized_missing_profile_diagnostic_rejects_only_pgo` |
| symlinked raw profile | Reject before merge | `symlinked_raw_profile_rejects_before_merge` |
| non-regular raw profile | Reject before merge | `non_regular_raw_profile_rejects_before_merge` |
| excessive raw profiles | Reject above 10,000 | `excessive_raw_profiles_reject_before_merge` |
| symlinked merged output | Reject before `llvm-profdata` | `preexisting_merged_profile_symlink_rejects_before_profdata_starts` |
| interrupted descendants | Kill and reap process group | `sigint_marks_the_run_interrupted_and_removes_the_workload_tree` |

Every supported case records all PGO phases, a matched schema-2 parity record,
strict merge arguments and seven screening samples. Every PGO-boundary
rejection persists one stable reason, gives PGO no screening samples, and leaves
built static strategies eligible. The EP-002 matrix helpers compare tracked
source, manifest and lockfile checksums before and after each new supported and
profile-boundary execution. The interrupt case stops the run and proves cleanup
independently; the symlink cases prove no path escape.
