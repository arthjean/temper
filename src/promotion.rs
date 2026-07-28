use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::anchored::AnchoredDirectory;
use crate::hash::sha256_file;
use crate::strategy::Strategy;

const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromotionErrorKind {
    Interrupted,
    InvalidSource,
    ChecksumMismatch,
    Filesystem,
}

#[derive(Debug)]
pub(crate) struct PromotionError {
    pub(crate) kind: PromotionErrorKind,
    pub(crate) message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PromotionRecord {
    pub(crate) source_strategy: Strategy,
    pub(crate) source_path: PathBuf,
    pub(crate) source_sha256: String,
    pub(crate) promoted_path: PathBuf,
    pub(crate) promoted_sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) permissions_mode: u32,
}

pub(crate) struct AnchoredArtifact {
    file: File,
    source_path: PathBuf,
    expected_sha256: String,
    size_bytes: u64,
    permissions_mode: u32,
    device: u64,
    inode: u64,
}

impl AnchoredArtifact {
    pub(crate) fn open(
        run_directory: &Path,
        source: &Path,
        expected_sha256: &str,
    ) -> Result<Self, PromotionError> {
        let canonical_run = fs::canonicalize(run_directory).map_err(|error| {
            invalid_source(format!(
                "Could not verify the run directory before confirmation: {error}"
            ))
        })?;
        if canonical_run != run_directory {
            return Err(invalid_source(
                "Run directory path changed after creation; confirmation was aborted.",
            ));
        }
        let confirmation_root = fs::canonicalize(run_directory.join("target/confirmation"))
            .map_err(|error| {
                invalid_source(format!(
                    "Confirmation target directory is unavailable: {error}"
                ))
            })?;
        let source_path = fs::canonicalize(source).map_err(|error| {
            invalid_source(format!(
                "Confirmation executable {} is unavailable: {error}",
                source.display()
            ))
        })?;
        if !confirmation_root.starts_with(run_directory)
            || !source_path.starts_with(&confirmation_root)
        {
            return Err(invalid_source(
                "Confirmation executable escaped its run-scoped target directory.",
            ));
        }
        let file = File::open(&source_path).map_err(|error| {
            invalid_source(format!(
                "Could not anchor confirmation executable {}: {error}",
                source_path.display()
            ))
        })?;
        let metadata = file.metadata().map_err(|error| {
            invalid_source(format!(
                "Could not inspect confirmation executable: {error}"
            ))
        })?;
        if !metadata.is_file() {
            return Err(invalid_source(
                "Confirmation executable is not a regular file.",
            ));
        }
        let actual_sha256 = sha256_open_file(&file)?;
        if actual_sha256 != expected_sha256 {
            return Err(checksum_error(
                "Confirmation executable changed after its Cargo build.",
            ));
        }
        Ok(Self {
            file,
            source_path,
            expected_sha256: expected_sha256.to_owned(),
            size_bytes: metadata.len(),
            permissions_mode: metadata.permissions().mode() & 0o7777,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.source_path
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), PromotionError> {
        let current_path = fs::canonicalize(&self.source_path).map_err(|error| {
            invalid_source(format!(
                "Confirmation executable disappeared during measurement: {error}"
            ))
        })?;
        let metadata = fs::metadata(&current_path).map_err(|error| {
            invalid_source(format!(
                "Could not re-inspect confirmation executable: {error}"
            ))
        })?;
        if current_path != self.source_path
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.len() != self.size_bytes
            || sha256_file(&current_path).map_err(|error| checksum_error(error.to_string()))?
                != self.expected_sha256
            || sha256_open_file(&self.file)? != self.expected_sha256
        {
            return Err(checksum_error(
                "Confirmation artifact identity or contents changed during measurement.",
            ));
        }
        Ok(())
    }

    fn reader(&self) -> Result<File, PromotionError> {
        let mut reader = self.file.try_clone().map_err(|error| {
            filesystem_error(format!("Could not clone anchored artifact handle: {error}"))
        })?;
        reader.seek(SeekFrom::Start(0)).map_err(|error| {
            filesystem_error(format!("Could not rewind anchored artifact: {error}"))
        })?;
        Ok(reader)
    }
}

pub(crate) fn promote_artifact(
    run_directory_path: &Path,
    run_directory: &AnchoredDirectory,
    source: &AnchoredArtifact,
    strategy: Strategy,
    is_interrupted: &dyn Fn() -> bool,
) -> Result<PromotionRecord, PromotionError> {
    promote_artifact_with_sync(
        run_directory_path,
        run_directory,
        source,
        strategy,
        is_interrupted,
        &|directory| {
            directory
                .sync()
                .map_err(|error| filesystem_error(error.message))
        },
    )
}

fn promote_artifact_with_sync(
    run_directory_path: &Path,
    run_directory: &AnchoredDirectory,
    source: &AnchoredArtifact,
    strategy: Strategy,
    is_interrupted: &dyn Fn() -> bool,
    sync_directory: &dyn Fn(&AnchoredDirectory) -> Result<(), PromotionError>,
) -> Result<PromotionRecord, PromotionError> {
    if is_interrupted() {
        return Err(interrupted_error());
    }
    source.verify_unchanged()?;
    let best_directory = run_directory
        .create_child("best", 0o700)
        .map_err(|error| filesystem_error(error.message))?;
    let temporary_name = format!(".artifact.tmp-{}", std::process::id());
    let mut destination = best_directory
        .create_file(&temporary_name, 0o600)
        .map_err(|error| filesystem_error(error.message))?;
    let copy_result = copy_and_sync(source, &mut destination, is_interrupted);
    if let Err(error) = copy_result {
        let _removed = best_directory.unlink(&temporary_name);
        return Err(error);
    }
    let promoted_sha256 = sha256_open_file(&destination)?;
    if promoted_sha256 != source.expected_sha256 {
        let _removed = best_directory.unlink(&temporary_name);
        return Err(checksum_error(
            "Copied artifact checksum did not match the confirmed candidate.",
        ));
    }
    if is_interrupted() {
        let _removed = best_directory.unlink(&temporary_name);
        return Err(interrupted_error());
    }
    best_directory
        .rename_noreplace(&temporary_name, "artifact")
        .map_err(|error| {
            let _removed = best_directory.unlink(&temporary_name);
            filesystem_error(format!(
                "Could not publish promoted artifact: {}",
                error.message
            ))
        })?;
    if is_interrupted() {
        let _removed = best_directory.unlink("artifact");
        let _synced = best_directory.sync();
        return Err(interrupted_error());
    }
    if let Err(error) = sync_directory(&best_directory) {
        return Err(rollback_published_artifact(
            &best_directory,
            run_directory,
            error,
            sync_directory,
        ));
    }
    if let Err(error) = sync_directory(run_directory) {
        return Err(rollback_published_artifact(
            &best_directory,
            run_directory,
            error,
            sync_directory,
        ));
    }

    Ok(PromotionRecord {
        source_strategy: strategy,
        source_path: source.source_path.clone(),
        source_sha256: source.expected_sha256.clone(),
        promoted_path: run_directory_path.join("best/artifact"),
        promoted_sha256,
        size_bytes: source.size_bytes,
        permissions_mode: source.permissions_mode,
    })
}

fn rollback_published_artifact(
    best_directory: &AnchoredDirectory,
    run_directory: &AnchoredDirectory,
    error: PromotionError,
    sync_directory: &dyn Fn(&AnchoredDirectory) -> Result<(), PromotionError>,
) -> PromotionError {
    let mut message = error.message;
    if let Err(cleanup_error) = best_directory.unlink("artifact") {
        message.push_str(" Artifact removal also failed: ");
        message.push_str(&cleanup_error.message);
        return filesystem_error(message);
    }
    message.push_str(" The published artifact was removed before reporting the failure.");
    for directory in [best_directory, run_directory] {
        if let Err(cleanup_error) = sync_directory(directory) {
            message.push_str(" Artifact removal sync also failed: ");
            message.push_str(&cleanup_error.message);
            break;
        }
    }
    filesystem_error(message)
}

pub(crate) fn discard_promoted_artifact(
    run_directory: &AnchoredDirectory,
) -> Result<(), PromotionError> {
    let best_directory = run_directory
        .create_child("best", 0o700)
        .map_err(|error| filesystem_error(error.message))?;
    best_directory
        .unlink("artifact")
        .map_err(|error| filesystem_error(error.message))?;
    best_directory
        .sync()
        .map_err(|error| filesystem_error(error.message))?;
    run_directory
        .sync()
        .map_err(|error| filesystem_error(error.message))
}

fn copy_and_sync(
    source: &AnchoredArtifact,
    destination: &mut File,
    is_interrupted: &dyn Fn() -> bool,
) -> Result<(), PromotionError> {
    let mut source_file = source.reader()?;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        if is_interrupted() {
            return Err(interrupted_error());
        }
        let bytes_read = source_file
            .read(&mut buffer)
            .map_err(|error| filesystem_error(format!("Artifact copy failed: {error}")))?;
        if bytes_read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..bytes_read])
            .map_err(|error| filesystem_error(format!("Artifact copy failed: {error}")))?;
    }
    destination
        .set_permissions(fs::Permissions::from_mode(source.permissions_mode))
        .map_err(|error| {
            filesystem_error(format!(
                "Could not preserve confirmed executable permissions: {error}"
            ))
        })?;
    destination
        .sync_all()
        .map_err(|error| filesystem_error(format!("Could not sync promoted artifact: {error}")))
}

fn sha256_open_file(file: &File) -> Result<String, PromotionError> {
    let mut reader = file.try_clone().map_err(|error| {
        checksum_error(format!("Could not clone artifact for hashing: {error}"))
    })?;
    reader.seek(SeekFrom::Start(0)).map_err(|error| {
        checksum_error(format!("Could not rewind artifact for hashing: {error}"))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .map_err(|error| checksum_error(format!("Could not hash artifact: {error}")))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn interrupted_error() -> PromotionError {
    PromotionError {
        kind: PromotionErrorKind::Interrupted,
        message: "Artifact promotion was interrupted; no latest pointer was changed.".to_owned(),
    }
}

fn invalid_source(message: impl Into<String>) -> PromotionError {
    PromotionError {
        kind: PromotionErrorKind::InvalidSource,
        message: message.into(),
    }
}

fn checksum_error(message: impl Into<String>) -> PromotionError {
    PromotionError {
        kind: PromotionErrorKind::ChecksumMismatch,
        message: message.into(),
    }
}

fn filesystem_error(message: impl Into<String>) -> PromotionError {
    PromotionError {
        kind: PromotionErrorKind::Filesystem,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnchoredArtifact, PromotionErrorKind, copy_and_sync, discard_promoted_artifact,
        filesystem_error, promote_artifact, promote_artifact_with_sync,
    };
    use crate::anchored::AnchoredDirectory;
    use crate::hash::sha256_file;
    use crate::strategy::Strategy;
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn confirmation_artifact() -> (tempfile::TempDir, AnchoredDirectory, AnchoredArtifact) {
        let run = tempfile::tempdir().expect("run directory");
        let confirmation = run.path().join("target/confirmation/thin-lto");
        fs::create_dir_all(&confirmation).expect("confirmation directory");
        let source = confirmation.join("candidate");
        fs::write(&source, b"confirmed artifact").expect("candidate");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).expect("permissions");
        let sha256 = sha256_file(&source).expect("checksum");
        let directory = AnchoredDirectory::open(run.path()).expect("anchor run");
        let artifact =
            AnchoredArtifact::open(run.path(), &source, &sha256).expect("anchor artifact");
        (run, directory, artifact)
    }

    #[test]
    fn copies_permissions_and_checksum_without_overwrite() {
        let (run, directory, artifact) = confirmation_artifact();
        let record = promote_artifact(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| false,
        )
        .expect("promotion");
        assert_eq!(record.promoted_sha256, record.source_sha256);
        assert_eq!(record.permissions_mode, 0o751);
        assert_eq!(
            fs::metadata(record.promoted_path)
                .expect("promoted metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o751
        );
    }

    #[test]
    fn interruption_removes_partial_output() {
        let (run, directory, artifact) = confirmation_artifact();
        let checks = Cell::new(0_u8);
        let result = promote_artifact(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| {
                checks.set(checks.get() + 1);
                checks.get() >= 3
            },
        );
        assert_eq!(
            result.expect_err("interrupted promotion").kind,
            PromotionErrorKind::Interrupted
        );
        assert!(!run.path().join("best/artifact").exists());
    }

    #[test]
    fn discards_a_committed_artifact_before_latest_publication() {
        let (run, directory, artifact) = confirmation_artifact();
        promote_artifact(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| false,
        )
        .expect("promotion");
        assert!(run.path().join("best/artifact").exists());

        discard_promoted_artifact(&directory).expect("discard promotion");

        assert!(!run.path().join("best/artifact").exists());
    }

    #[test]
    fn source_checksum_change_rejects_before_copy() {
        let (run, directory, artifact) = confirmation_artifact();
        fs::write(artifact.path(), b"changed confirmed artifact").expect("change candidate");

        let error = promote_artifact(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| false,
        )
        .expect_err("checksum change must reject");

        assert_eq!(error.kind, PromotionErrorKind::ChecksumMismatch);
        assert!(!run.path().join("best/artifact").exists());
    }

    #[test]
    fn rename_failure_preserves_the_existing_destination() {
        let (run, directory, artifact) = confirmation_artifact();
        let best = run.path().join("best");
        fs::create_dir(&best).expect("create best directory");
        fs::write(best.join("artifact"), b"existing artifact").expect("seed artifact");

        let error = promote_artifact(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| false,
        )
        .expect_err("rename must not overwrite");

        assert_eq!(error.kind, PromotionErrorKind::Filesystem);
        assert_eq!(
            fs::read(best.join("artifact")).expect("read existing artifact"),
            b"existing artifact"
        );
        assert!(
            fs::read_dir(best)
                .expect("read best directory")
                .all(|entry| !entry
                    .expect("best entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".artifact.tmp-"))
        );
    }

    #[test]
    fn full_destination_reports_a_filesystem_copy_failure() {
        let (_run, _directory, artifact) = confirmation_artifact();
        let mut full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open /dev/full");

        let error = copy_and_sync(&artifact, &mut full, &|| false)
            .expect_err("/dev/full must reject writes");

        assert_eq!(error.kind, PromotionErrorKind::Filesystem);
        assert!(error.message.contains("Artifact copy failed"));
    }

    #[test]
    fn best_directory_sync_failure_removes_the_renamed_artifact() {
        assert_sync_failure_rolls_back(0);
    }

    #[test]
    fn run_directory_sync_failure_removes_the_renamed_artifact() {
        assert_sync_failure_rolls_back(1);
    }

    fn assert_sync_failure_rolls_back(failing_call: u8) {
        let (run, directory, artifact) = confirmation_artifact();
        let sync_calls = Cell::new(0_u8);

        let error = promote_artifact_with_sync(
            run.path(),
            &directory,
            &artifact,
            Strategy::ThinLto,
            &|| false,
            &|anchored| {
                let call = sync_calls.get();
                sync_calls.set(call + 1);
                if call == failing_call {
                    Err(filesystem_error("injected directory sync failure"))
                } else {
                    anchored
                        .sync()
                        .map_err(|error| filesystem_error(error.message))
                }
            },
        )
        .expect_err("directory sync failure must reject promotion");

        assert_eq!(error.kind, PromotionErrorKind::Filesystem);
        assert!(!run.path().join("best/artifact").exists());
        assert!(
            fs::read_dir(run.path().join("best"))
                .expect("best directory")
                .all(|entry| !entry
                    .expect("best entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".artifact.tmp-"))
        );
    }
}
