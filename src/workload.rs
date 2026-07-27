use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{Result as TemperResult, TemperError};
use crate::measurement::{
    self, BOOTSTRAP_SEED, CONFIRMATION_PAIR_COUNT, ConfirmationResult, SCREENING_SAMPLE_COUNT,
    ScreeningResult, WARMUP_COUNT,
};

const OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const MONITOR_INTERVAL: Duration = Duration::from_millis(5);
const TERMINATION_GRACE: Duration = Duration::from_millis(250);
const REAP_DEADLINE: Duration = Duration::from_secs(2);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkloadFailureKind {
    InvalidCandidate,
    SpawnFailed,
    NonzeroExit,
    Timeout,
    Interrupted,
    OutputLimit,
    CaptureFailed,
    MeasurementFailed,
}

#[derive(Debug)]
pub(crate) struct WorkloadFailure {
    pub(crate) kind: WorkloadFailureKind,
    pub(crate) message: String,
    pub(crate) bounded_diagnostics: String,
    pub(crate) diagnostics_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmationStep {
    Baseline,
    Candidate,
    Measurement,
}

#[derive(Debug)]
pub(crate) struct ConfirmationFailure {
    pub(crate) step: ConfirmationStep,
    pub(crate) failure: WorkloadFailure,
}

impl WorkloadFailure {
    fn new(kind: WorkloadFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            bounded_diagnostics: String::new(),
            diagnostics_truncated: false,
        }
    }

    fn with_captures(
        kind: WorkloadFailureKind,
        message: impl Into<String>,
        stdout: &CapturedStream,
        stderr: &CapturedStream,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            bounded_diagnostics: captured_diagnostics(stdout, stderr),
            diagnostics_truncated: stdout.truncated || stderr.truncated,
        }
    }

    fn measurement(error: measurement::MeasurementError) -> Self {
        Self::new(
            WorkloadFailureKind::MeasurementFailed,
            format!("measurement-v1 rejected the sample set: {error}"),
        )
    }

    pub(crate) fn unstable_baseline(relative_mad: f64) -> Self {
        Self::new(
            WorkloadFailureKind::MeasurementFailed,
            format!(
                "Baseline screening was unstable (relative MAD {relative_mad:.6}); no candidate can be compared safely."
            ),
        )
    }
}

impl fmt::Display for WorkloadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkloadFailure {}

#[derive(Debug)]
pub(crate) struct InvocationResult {
    pub(crate) duration_ns: u64,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkloadSpec {
    executable: OsString,
    arguments: Vec<OsString>,
    workspace_root: PathBuf,
    timeout: Duration,
}

impl WorkloadSpec {
    pub(crate) fn new(
        argv: &[OsString],
        workspace_root: &Path,
        timeout_seconds: u64,
    ) -> Result<Self, WorkloadFailure> {
        Self::with_timeout(argv, workspace_root, Duration::from_secs(timeout_seconds))
    }

    fn with_timeout(
        argv: &[OsString],
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, WorkloadFailure> {
        let (executable, arguments) = argv.split_first().ok_or_else(|| {
            WorkloadFailure::new(
                WorkloadFailureKind::SpawnFailed,
                "The workload executable is missing.",
            )
        })?;
        let workspace_root = std::fs::canonicalize(workspace_root).map_err(|error| {
            WorkloadFailure::new(
                WorkloadFailureKind::SpawnFailed,
                format!(
                    "The Cargo workspace root {} could not be canonicalized: {error}",
                    workspace_root.display()
                ),
            )
        })?;

        Ok(Self {
            executable: executable.clone(),
            arguments: arguments.to_vec(),
            workspace_root,
            timeout,
        })
    }

    pub(crate) fn invoke(&self, candidate: &Path) -> Result<InvocationResult, WorkloadFailure> {
        self.invoke_with_environment(candidate, &[])
    }

    pub(crate) fn invoke_with_environment(
        &self,
        candidate: &Path,
        environment: &[(&str, &OsStr)],
    ) -> Result<InvocationResult, WorkloadFailure> {
        let candidate = canonical_candidate(candidate)?;
        let started = Instant::now();
        let mut child = Command::new(&self.executable)
            .args(&self.arguments)
            .current_dir(&self.workspace_root)
            .env("TEMPER_BINARY", &candidate)
            .envs(environment.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| {
                WorkloadFailure::new(
                    WorkloadFailureKind::SpawnFailed,
                    format!(
                        "Workload executable {} could not start: {error}",
                        Path::new(&self.executable).display()
                    ),
                )
            })?;
        let process_group = i32::try_from(child.id()).map_err(|_| {
            WorkloadFailure::new(
                WorkloadFailureKind::SpawnFailed,
                "Workload process ID did not fit the Linux process-group identifier.",
            )
        })?;
        let stdout = take_pipe(&mut child, process_group, StreamKind::Stdout)?;
        let stderr = take_pipe(&mut child, process_group, StreamKind::Stderr)?;
        let (event_sender, event_receiver) = mpsc::channel();
        let stdout_reader = spawn_capture(stdout, StreamKind::Stdout, event_sender.clone());
        let stderr_reader = spawn_capture(stderr, StreamKind::Stderr, event_sender);

        let completion = monitor_child(
            &mut child,
            process_group,
            started,
            self.timeout,
            &event_receiver,
        );
        let stdout = join_capture(stdout_reader, StreamKind::Stdout)?;
        let stderr = join_capture(stderr_reader, StreamKind::Stderr)?;

        if let Some(error) = stdout.error.as_ref().or(stderr.error.as_ref()) {
            return Err(WorkloadFailure::with_captures(
                WorkloadFailureKind::CaptureFailed,
                format!("Workload output could not be captured: {error}"),
                &stdout,
                &stderr,
            ));
        }
        if stdout.truncated || stderr.truncated {
            let stream = if stdout.truncated { "stdout" } else { "stderr" };
            return Err(WorkloadFailure::with_captures(
                WorkloadFailureKind::OutputLimit,
                format!("Workload {stream} exceeded the 1 MiB limit; the artifact was rejected."),
                &stdout,
                &stderr,
            ));
        }

        let status = match completion {
            Completion::Exited(status) => status,
            Completion::Terminated {
                kind,
                message,
                termination_error,
            } => {
                let message = match termination_error {
                    Some(error) => format!("{message} Process-group cleanup reported: {error}"),
                    None => message,
                };
                return Err(WorkloadFailure::with_captures(
                    kind, message, &stdout, &stderr,
                ));
            }
        };
        if !status.success() {
            return Err(WorkloadFailure::with_captures(
                WorkloadFailureKind::NonzeroExit,
                format!("Workload exited with {status}; exit status zero is required."),
                &stdout,
                &stderr,
            ));
        }

        let duration_ns = u64::try_from(started.elapsed().as_nanos()).map_err(|_| {
            WorkloadFailure::new(
                WorkloadFailureKind::MeasurementFailed,
                "Workload duration exceeded the measurement-v1 integer range.",
            )
        })?;
        if duration_ns == 0 {
            return Err(WorkloadFailure::new(
                WorkloadFailureKind::MeasurementFailed,
                "The monotonic workload duration was zero.",
            ));
        }

        Ok(InvocationResult {
            duration_ns,
            stdout: stdout.text(),
            stderr: stderr.text(),
        })
    }

    pub(crate) fn screen(&self, candidate: &Path) -> Result<ScreeningResult, WorkloadFailure> {
        for _ in 0..WARMUP_COUNT {
            self.invoke(candidate)?;
        }
        let mut samples = Vec::with_capacity(SCREENING_SAMPLE_COUNT);
        for _ in 0..SCREENING_SAMPLE_COUNT {
            let invocation = self.invoke(candidate)?;
            samples.push(invocation.duration_ns);
            drop((invocation.stdout, invocation.stderr));
        }
        measurement::screening(&samples).map_err(WorkloadFailure::measurement)
    }

    pub(crate) fn confirm(
        &self,
        baseline: &Path,
        candidate: &Path,
        minimum_improvement_percent: f64,
    ) -> Result<ConfirmationResult, ConfirmationFailure> {
        let mut baseline_samples = Vec::with_capacity(CONFIRMATION_PAIR_COUNT);
        let mut candidate_samples = Vec::with_capacity(CONFIRMATION_PAIR_COUNT);
        for pair_index in 0..CONFIRMATION_PAIR_COUNT {
            if pair_index % 2 == 0 {
                baseline_samples.push(
                    self.invoke(baseline)
                        .map_err(|failure| ConfirmationFailure {
                            step: ConfirmationStep::Baseline,
                            failure,
                        })?
                        .duration_ns,
                );
                candidate_samples.push(
                    self.invoke(candidate)
                        .map_err(|failure| ConfirmationFailure {
                            step: ConfirmationStep::Candidate,
                            failure,
                        })?
                        .duration_ns,
                );
            } else {
                candidate_samples.push(
                    self.invoke(candidate)
                        .map_err(|failure| ConfirmationFailure {
                            step: ConfirmationStep::Candidate,
                            failure,
                        })?
                        .duration_ns,
                );
                baseline_samples.push(
                    self.invoke(baseline)
                        .map_err(|failure| ConfirmationFailure {
                            step: ConfirmationStep::Baseline,
                            failure,
                        })?
                        .duration_ns,
                );
            }
        }
        measurement::confirmation(
            &baseline_samples,
            &candidate_samples,
            minimum_improvement_percent,
            BOOTSTRAP_SEED,
        )
        .map_err(WorkloadFailure::measurement)
        .map_err(|failure| ConfirmationFailure {
            step: ConfirmationStep::Measurement,
            failure,
        })
    }
}

pub(crate) fn install_interrupt_handler() -> TemperResult<()> {
    INTERRUPTED.store(false, Ordering::SeqCst);
    // The handler only writes a lock-free atomic, which is async-signal-safe.
    let previous = unsafe {
        libc::signal(
            libc::SIGINT,
            record_interrupt as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        Err(TemperError::new(format!(
            "Could not install the SIGINT handler: {}",
            io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

extern "C" fn record_interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn canonical_candidate(candidate: &Path) -> Result<PathBuf, WorkloadFailure> {
    let candidate = std::fs::canonicalize(candidate).map_err(|error| {
        WorkloadFailure::new(
            WorkloadFailureKind::InvalidCandidate,
            format!(
                "Candidate executable {} could not be canonicalized: {error}",
                candidate.display()
            ),
        )
    })?;
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(WorkloadFailure::new(
            WorkloadFailureKind::InvalidCandidate,
            format!(
                "Candidate executable {} is not a file.",
                candidate.display()
            ),
        ))
    }
}

fn take_pipe(
    child: &mut Child,
    process_group: i32,
    stream: StreamKind,
) -> Result<Pipe, WorkloadFailure> {
    let pipe = match stream {
        StreamKind::Stdout => child.stdout.take().map(Pipe::Stdout),
        StreamKind::Stderr => child.stderr.take().map(Pipe::Stderr),
    };
    match pipe {
        Some(pipe) => Ok(pipe),
        None => {
            let _cleanup = terminate_process_group(child, process_group);
            Err(WorkloadFailure::new(
                WorkloadFailureKind::CaptureFailed,
                format!("Workload {} pipe was unavailable.", stream.label()),
            ))
        }
    }
}

enum Pipe {
    Stdout(std::process::ChildStdout),
    Stderr(std::process::ChildStderr),
}

impl Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Stdout(pipe) => pipe.read(buffer),
            Self::Stderr(pipe) => pipe.read(buffer),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
}

impl StreamKind {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

enum ReaderEvent {
    Finished(StreamKind),
    Limit(StreamKind),
    Failed(StreamKind),
}

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
    error: Option<String>,
}

impl CapturedStream {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

fn spawn_capture(
    reader: impl Read + Send + 'static,
    stream: StreamKind,
    events: mpsc::Sender<ReaderEvent>,
) -> thread::JoinHandle<CapturedStream> {
    thread::spawn(move || capture_bounded(reader, stream, &events))
}

fn capture_bounded(
    mut reader: impl Read,
    stream: StreamKind,
    events: &mpsc::Sender<ReaderEvent>,
) -> CapturedStream {
    let mut bytes = Vec::with_capacity(OUTPUT_LIMIT_BYTES);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _sent = events.send(ReaderEvent::Finished(stream));
                return CapturedStream {
                    bytes,
                    truncated: false,
                    error: None,
                };
            }
            Ok(bytes_read) => {
                let remaining = OUTPUT_LIMIT_BYTES.saturating_sub(bytes.len());
                if bytes_read > remaining {
                    bytes.extend_from_slice(&buffer[..remaining]);
                    let _sent = events.send(ReaderEvent::Limit(stream));
                    return CapturedStream {
                        bytes,
                        truncated: true,
                        error: None,
                    };
                }
                bytes.extend_from_slice(&buffer[..bytes_read]);
            }
            Err(error) => {
                let _sent = events.send(ReaderEvent::Failed(stream));
                return CapturedStream {
                    bytes,
                    truncated: false,
                    error: Some(error.to_string()),
                };
            }
        }
    }
}

fn join_capture(
    handle: thread::JoinHandle<CapturedStream>,
    stream: StreamKind,
) -> Result<CapturedStream, WorkloadFailure> {
    handle.join().map_err(|_| {
        WorkloadFailure::new(
            WorkloadFailureKind::CaptureFailed,
            format!("Workload {} capture thread terminated.", stream.label()),
        )
    })
}

enum Completion {
    Exited(ExitStatus),
    Terminated {
        kind: WorkloadFailureKind,
        message: String,
        termination_error: Option<String>,
    },
}

fn monitor_child(
    child: &mut Child,
    process_group: i32,
    started: Instant,
    timeout: Duration,
    events: &mpsc::Receiver<ReaderEvent>,
) -> Completion {
    let mut successful_exit = None;
    let mut stdout_finished = false;
    let mut stderr_finished = false;

    loop {
        while let Ok(event) = events.try_recv() {
            match event {
                ReaderEvent::Finished(stream) => match stream {
                    StreamKind::Stdout => stdout_finished = true,
                    StreamKind::Stderr => stderr_finished = true,
                },
                ReaderEvent::Limit(stream) => {
                    return terminated(
                        child,
                        process_group,
                        WorkloadFailureKind::OutputLimit,
                        format!(
                            "Workload {} exceeded its bound; the artifact was rejected.",
                            stream.label()
                        ),
                    );
                }
                ReaderEvent::Failed(stream) => {
                    return terminated(
                        child,
                        process_group,
                        WorkloadFailureKind::CaptureFailed,
                        format!("Workload {} capture failed.", stream.label()),
                    );
                }
            }
        }
        if INTERRUPTED.load(Ordering::SeqCst) {
            return terminated(
                child,
                process_group,
                WorkloadFailureKind::Interrupted,
                "Workload was interrupted by SIGINT; the artifact was rejected.",
            );
        }
        if successful_exit.is_none() {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => successful_exit = Some(status),
                Ok(Some(status)) => {
                    return terminated(
                        child,
                        process_group,
                        WorkloadFailureKind::NonzeroExit,
                        format!("Workload exited with {status}; exit status zero is required."),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    return terminated(
                        child,
                        process_group,
                        WorkloadFailureKind::CaptureFailed,
                        format!("Workload status could not be observed: {error}"),
                    );
                }
            }
        }
        if stdout_finished && stderr_finished {
            match process_group_exists(process_group) {
                Ok(false) => {
                    if let Some(status) = successful_exit {
                        return Completion::Exited(status);
                    }
                }
                Ok(true) => {}
                Err(error) => {
                    return terminated(
                        child,
                        process_group,
                        WorkloadFailureKind::CaptureFailed,
                        format!("Workload process-group state could not be observed: {error}"),
                    );
                }
            }
        }
        if started.elapsed() >= timeout {
            return terminated(
                child,
                process_group,
                WorkloadFailureKind::Timeout,
                format!(
                    "Workload exceeded its {} second timeout; the artifact was rejected.",
                    timeout.as_secs_f64()
                ),
            );
        }
        thread::sleep(MONITOR_INTERVAL);
    }
}

fn terminated(
    child: &mut Child,
    process_group: i32,
    kind: WorkloadFailureKind,
    message: impl Into<String>,
) -> Completion {
    Completion::Terminated {
        kind,
        message: message.into(),
        termination_error: terminate_process_group(child, process_group)
            .err()
            .map(|error| error.to_string()),
    }
}

fn terminate_process_group(child: &mut Child, process_group: i32) -> io::Result<()> {
    send_group_signal(process_group, libc::SIGTERM)?;
    let grace_started = Instant::now();
    while grace_started.elapsed() < TERMINATION_GRACE && process_group_exists(process_group)? {
        let _status = child.try_wait()?;
        thread::sleep(MONITOR_INTERVAL);
    }
    if process_group_exists(process_group)? {
        send_group_signal(process_group, libc::SIGKILL)?;
    }

    let reap_started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if reap_started.elapsed() >= REAP_DEADLINE {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "direct workload child was not reaped within 2 seconds after escalation",
            ));
        }
        thread::sleep(MONITOR_INTERVAL);
    }
}

fn send_group_signal(process_group: i32, signal: libc::c_int) -> io::Result<()> {
    // A negative PID targets exactly the dedicated child process group.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_exists(process_group: i32) -> io::Result<bool> {
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

fn captured_diagnostics(stdout: &CapturedStream, stderr: &CapturedStream) -> String {
    let stdout = redact_environment_lines(&stdout.text());
    let stderr = redact_environment_lines(&stderr.text());
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

fn redact_environment_lines(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.split_once('=').is_some_and(|(key, _)| !key.is_empty()) {
                "<redacted environment entry>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{WorkloadFailureKind, WorkloadSpec};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    fn argv(values: &[&OsStr]) -> Vec<OsString> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn test_spec(values: &[&OsStr], workspace_root: &Path, timeout: Duration) -> WorkloadSpec {
        WorkloadSpec::with_timeout(&argv(values), workspace_root, timeout)
            .expect("valid workload specification")
    }

    #[test]
    fn passes_literal_arguments_without_shell_expansion() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let executable = root.path().join("printf tool");
        symlink("/usr/bin/printf", &executable).expect("create spaced executable path");
        let values = [
            executable.as_os_str(),
            OsStr::new("%s\n"),
            OsStr::new("space value"),
            OsStr::new("'quote\""),
            OsStr::new("*"),
            OsStr::new("$HOME"),
        ];
        let spec = test_spec(&values, root.path(), Duration::from_secs(2));
        let result = spec.invoke(Path::new("/bin/true")).expect("valid workload");
        assert_eq!(result.stdout, "space value\n'quote\"\n*\n$HOME\n");
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn sets_candidate_and_workspace_for_the_child() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let candidate = fs::canonicalize("/bin/true").expect("canonical candidate");
        let root_path = root.path().as_os_str();
        let candidate_path = candidate.as_os_str();
        let values = [
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("test \"$PWD\" = \"$1\" && test \"$TEMPER_BINARY\" = \"$2\""),
            OsStr::new("temper-test"),
            root_path,
            candidate_path,
        ];
        let spec = test_spec(&values, root.path(), Duration::from_secs(2));
        spec.invoke(&candidate).expect("child contract holds");
    }

    #[test]
    fn screening_runs_two_warmups_and_seven_recorded_samples() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let counter = root.path().join("counter");
        let values = [
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("printf 'sample\\n' >> \"$1\""),
            OsStr::new("temper-test"),
            counter.as_os_str(),
        ];
        let spec = test_spec(&values, root.path(), Duration::from_secs(2));
        let result = spec
            .screen(Path::new("/bin/true"))
            .expect("valid screening");
        assert_eq!(result.sample_durations_ns.len(), 7);
        assert_eq!(
            fs::read_to_string(counter)
                .expect("read invocation counter")
                .lines()
                .count(),
            9
        );
    }

    #[test]
    fn rejects_nonzero_and_output_limited_workloads() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let nonzero = test_spec(
            &[OsStr::new("/bin/false")],
            root.path(),
            Duration::from_secs(2),
        );
        assert_eq!(
            nonzero
                .invoke(Path::new("/bin/true"))
                .expect_err("nonzero must fail")
                .kind,
            WorkloadFailureKind::NonzeroExit
        );

        let output_limited = test_spec(
            &[
                OsStr::new("/usr/bin/head"),
                OsStr::new("-c"),
                OsStr::new("1048577"),
                OsStr::new("/dev/zero"),
            ],
            root.path(),
            Duration::from_secs(2),
        );
        assert_eq!(
            output_limited
                .invoke(Path::new("/bin/true"))
                .expect_err("output limit must fail")
                .kind,
            WorkloadFailureKind::OutputLimit
        );

        let stderr_limited = test_spec(
            &[
                OsStr::new("/bin/sh"),
                OsStr::new("-c"),
                OsStr::new("head -c 1048577 /dev/zero >&2"),
            ],
            root.path(),
            Duration::from_secs(2),
        );
        assert_eq!(
            stderr_limited
                .invoke(Path::new("/bin/true"))
                .expect_err("stderr limit must fail")
                .kind,
            WorkloadFailureKind::OutputLimit
        );
    }

    #[test]
    fn failure_diagnostics_redact_inherited_environment_entries() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let spec = test_spec(
            &[
                OsStr::new("/bin/sh"),
                OsStr::new("-c"),
                OsStr::new("env; exit 1"),
            ],
            root.path(),
            Duration::from_secs(2),
        );
        let failure = spec
            .invoke(Path::new("/bin/true"))
            .expect_err("nonzero environment workload must fail");
        assert_eq!(failure.kind, WorkloadFailureKind::NonzeroExit);
        assert!(
            failure
                .bounded_diagnostics
                .contains("<redacted environment entry>")
        );
        assert!(!failure.bounded_diagnostics.contains("PATH="));
        assert!(!failure.bounded_diagnostics.contains("TEMPER_BINARY="));
    }

    #[test]
    fn timeout_terminates_and_reaps_a_forked_descendant() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let marker = root.path().join("child-pid");
        let values = [
            OsStr::new("/bin/sh"),
            OsStr::new("-c"),
            OsStr::new("sleep 30 & child=$!; printf '%s' \"$child\" > \"$1\""),
            OsStr::new("temper-test"),
            marker.as_os_str(),
        ];
        let spec = test_spec(&values, root.path(), Duration::from_millis(50));
        assert_eq!(
            spec.invoke(Path::new("/bin/true"))
                .expect_err("timeout must fail")
                .kind,
            WorkloadFailureKind::Timeout
        );

        let child_pid = fs::read_to_string(marker)
            .expect("descendant marker")
            .trim()
            .to_owned();
        let process_path = format!("/proc/{child_pid}");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Path::new(&process_path).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !Path::new(&process_path).exists(),
            "forked descendant {child_pid} survived process-group cleanup"
        );
    }
}
