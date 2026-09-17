//! Approved local computational execution (SPEC §8, §9.3, §13 step 4).
//!
//! Generated code runs in a bounded, OS-contained workspace with a runnable
//! check. Every execution records exact inputs, code, environment, commands,
//! outputs, and a rerun command. A failed execution can never become
//! supporting evidence. This function proposes records; the caller commits
//! them through the controlled write path.

use crate::records::*;
use crate::store::{Store, write_atomic};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use time::OffsetDateTime;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// A request to execute an approved analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecRequest {
    /// Program to run, which must be on the allowed-tools list.
    pub program: String,
    pub args: Vec<String>,
    /// Input files copied read-only into the workspace.
    pub inputs: Vec<String>,
    /// Seconds before the process tree is terminated.
    pub timeout_secs: u64,
    /// Maximum bytes captured from each of stdout and stderr.
    pub max_output_bytes: usize,
    /// Deterministic seed passed through to the analysis, where applicable.
    pub seed: Option<u64>,
}

impl ExecRequest {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            inputs: vec![],
            timeout_secs: 120,
            max_output_bytes: 1 << 20,
            seed: None,
        }
    }

    /// The exact payload an approval binds to (SPEC §11).
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "program": self.program,
            "args": self.args,
            "inputs": self.inputs,
            "timeout_secs": self.timeout_secs,
            "seed": self.seed,
        })
    }

    pub fn payload_hash(&self) -> ContentHash {
        ContentHash::of_json(&self.payload()).expect("payload is plain JSON")
    }

    /// Human-readable command, for the reproducibility record.
    ///
    /// Secrets are never part of a request, so nothing is redacted here; the
    /// environment is passed separately and not recorded verbatim.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What an execution produced, as proposed records.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub record: ExecutionRecord,
    /// Evidence, present only when the execution actually succeeded.
    pub evidence: Option<Evidence>,
    pub artifacts: Vec<Artifact>,
    /// How the process was contained.
    pub containment: String,
}

impl ExecOutcome {
    /// Whether this execution may be cited in support of a claim.
    pub fn usable_as_evidence(&self) -> bool {
        self.record.may_support_claims() && self.evidence.is_some()
    }
}

/// OS-level containment available on this host (SPEC §9.3).
///
/// "Unsupported containment is an unavailable execution capability, not
/// permission to run uncontained."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// macOS `sandbox-exec` with a profile denying network and writes outside
    /// the workspace.
    MacOsSandbox,
    /// Linux bubblewrap: read-only root, workspace bound writable, no network,
    /// own PID namespace.
    Bubblewrap(PathBuf),
}

impl Containment {
    pub fn describe(&self) -> String {
        match self {
            Self::MacOsSandbox => {
                "macOS sandbox-exec (no network; writes confined to workspace)".into()
            }
            Self::Bubblewrap(p) => format!(
                "bubblewrap {} (read-only root; workspace writable; no network; own pid namespace)",
                p.display()
            ),
        }
    }
}

/// Detect a supported containment mechanism.
pub fn detect_containment() -> Option<Containment> {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file() {
        return Some(Containment::MacOsSandbox);
    }
    if cfg!(target_os = "linux")
        && let Some(path) = find_on_path("bwrap")
    {
        return Some(Containment::Bubblewrap(path));
    }
    None
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Build the contained command line for `program args` inside `workspace`.
fn contained_command(
    containment: &Containment,
    workspace: &Path,
    program: &str,
    args: &[String],
) -> Result<tokio::process::Command> {
    let ws = workspace.to_string_lossy();
    if ws.contains('"') || ws.contains('\\') {
        return Err(Error::validation(
            "workspace path contains characters the sandbox profile cannot quote",
        ));
    }

    Ok(match containment {
        Containment::MacOsSandbox => {
            let profile = format!(
                "(version 1) (allow default) (deny network*) (deny file-write*) \
                 (allow file-write* (subpath \"{ws}\")) \
                 (allow file-write* (literal \"/dev/null\"))"
            );
            let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
            c.arg("-p").arg(profile).arg(program).args(args);
            c
        }
        Containment::Bubblewrap(bwrap) => {
            let mut c = tokio::process::Command::new(bwrap);
            c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
                .args(["--tmpfs", "/tmp"])
                .arg("--bind")
                .arg(workspace)
                .arg(workspace)
                .args(["--unshare-net", "--unshare-pid", "--die-with-parent"])
                .arg("--chdir")
                .arg(workspace)
                .arg("--")
                .arg(program)
                .args(args);
            c
        }
    })
}

/// Execute an approved analysis in an isolated, contained workspace.
///
/// The caller must have verified permissions and a matching approval; this
/// enforces the execute permission, the tool allowlist, and OS containment as
/// a second gate. Nothing is written to the store: the returned records are
/// committed by the caller with the task outcome (SPEC §9.2).
pub async fn execute_analysis(
    store: &Store,
    run_id: &RunId,
    task_id: &TaskId,
    request: &ExecRequest,
    permissions: &Permissions,
    cancel: &CancellationToken,
) -> Result<ExecOutcome> {
    if !permissions.execute {
        return Err(Error::permission(
            "this run does not permit local execution",
        ));
    }
    if !permissions.allows_tool(&request.program) {
        return Err(Error::permission(format!(
            "{} is not in this task's approved tool list",
            request.program
        )));
    }
    let Some(containment) = detect_containment() else {
        return Err(Error::permission(
            "no OS-level containment is available on this host (macOS sandbox-exec or \
             Linux bubblewrap); execution is an unavailable capability, not permission \
             to run uncontained (SPEC §9.3)",
        ));
    };

    // A task-owned workspace: concurrent tasks never share a writable
    // directory, and raw inputs are copied in rather than written through.
    let workspace = store
        .artifact_dir(run_id)
        .join(format!("workspace-{task_id}"));
    tokio::fs::create_dir_all(workspace.join("tmp")).await?;
    let workspace = tokio::fs::canonicalize(&workspace).await?;

    let mut input_hashes = Vec::new();
    for input in &request.inputs {
        let src = Path::new(input);
        // Inputs are limited to the paths this run may read (SPEC §12): a
        // model-proposed request cannot pull arbitrary files into a workspace.
        if !permitted_to_read(src, permissions) {
            return Err(Error::permission(format!(
                "execution input {input} is outside the permitted read paths"
            )));
        }
        let bytes = tokio::fs::read(src)
            .await
            .map_err(|e| Error::validation(format!("cannot read execution input {input}: {e}")))?;
        input_hashes.push(ContentHash::of_bytes(&bytes));

        let name = src
            .file_name()
            .ok_or_else(|| Error::validation(format!("input {input} has no file name")))?;
        // Copy, never move or modify: raw data stays untouched (SPEC §8).
        tokio::fs::write(workspace.join(name), &bytes).await?;
    }

    let started_at = OffsetDateTime::now_utc();

    let mut command = contained_command(&containment, &workspace, &request.program, &request.args)?;
    command
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A clean environment: nothing from the parent process leaks in, so a
        // credential in Uruk's environment cannot reach the subprocess.
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", workspace.to_string_lossy().to_string())
        .env(
            "TMPDIR",
            workspace.join("tmp").to_string_lossy().to_string(),
        );

    if let Some(seed) = request.seed {
        command.env("URUK_SEED", seed.to_string());
    }

    // A new process group, so the whole tree can be terminated. Dropping a
    // Rust future is not process cleanup (SPEC §9.3).
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|e| Error::validation(format!("cannot start {}: {e}", request.program)))?;

    let pid = child.id();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let limit = request.max_output_bytes;

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.take(limit as u64).read_to_end(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.take(limit as u64).read_to_end(&mut buf).await;
        }
        buf
    });

    let timeout = std::time::Duration::from_secs(request.timeout_secs);
    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            terminate(&mut child, pid).await;
            return Err(Error::Cancelled);
        }
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(status)) => Some(status),
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => {
                terminate(&mut child, pid).await;
                None
            }
        }
    };

    let finished_at = OffsetDateTime::now_utc();
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    let timed_out = status.is_none();
    let exit_status = status.and_then(|s| s.code()).unwrap_or(-1);

    let stdout_artifact =
        write_artifact(store, run_id, task_id, "stdout.txt", &stdout, "text/plain").await?;
    let stderr_artifact =
        write_artifact(store, run_id, task_id, "stderr.txt", &stderr, "text/plain").await?;

    // Validate outputs: partial or malformed output fails rather than being
    // promoted to an observation (SPEC §8).
    let (outputs_valid, validation_note) = if timed_out {
        (
            false,
            Some(format!(
                "execution exceeded its {}s limit and was terminated; \
                 any partial output is incomplete",
                request.timeout_secs
            )),
        )
    } else if exit_status != 0 {
        (
            false,
            Some(format!("process exited with status {exit_status}")),
        )
    } else if stdout.is_empty() {
        (
            false,
            Some("execution succeeded but produced no output to interpret".to_string()),
        )
    } else if stdout.len() >= limit {
        (
            false,
            Some(format!(
                "output reached the {limit}-byte capture limit and is truncated; \
                 a truncated result cannot be interpreted as complete"
            )),
        )
    } else {
        (true, None)
    };

    let command_line = request.command_line();
    let record = ExecutionRecord {
        command: command_line.clone(),
        environment: vec![
            format!("program={}", request.program),
            format!("workspace={}", workspace.display()),
            format!("timeout_secs={}", request.timeout_secs),
            format!("containment={}", containment.describe()),
        ],
        parameters: request.args.clone(),
        seed: request.seed,
        // The code that ran is the command line itself (inline scripts) plus
        // the hashed inputs (script files supplied as inputs).
        code_hash: Some(ContentHash::of_str(&command_line)),
        input_hashes,
        started_at,
        finished_at,
        exit_status,
        stdout_artifact: Some(stdout_artifact.id.clone()),
        stderr_artifact: Some(stderr_artifact.id.clone()),
        output_artifacts: vec![],
        rerun: Ok(format!("cd {} && {}", workspace.display(), command_line)),
        outputs_valid,
        validation_note,
    };

    // Evidence exists only for an execution that actually succeeded.
    let evidence = record.may_support_claims().then(|| Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: EvidenceKind::ComputationalResult,
        content: String::from_utf8_lossy(&stdout).into_owned(),
        method: command_line.clone(),
        inputs: request.inputs.clone(),
        source_ids: vec![],
        artifact_ids: vec![stdout_artifact.id.clone(), stderr_artifact.id.clone()],
        limitations: Some(format!(
            "single execution in an isolated workspace; reproduce with: cd {} && {}",
            workspace.display(),
            command_line
        )),
        produced_by: Some(task_id.clone()),
        created_at: finished_at,
    });

    Ok(ExecOutcome {
        record,
        evidence,
        artifacts: vec![stdout_artifact, stderr_artifact],
        containment: containment.describe(),
    })
}

/// Whether `path` lies under one of the run's readable paths.
fn permitted_to_read(path: &Path, permissions: &Permissions) -> bool {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    permissions.read_paths.iter().any(|p| {
        let base = Path::new(p)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(p).to_path_buf());
        canonical.starts_with(&base)
    })
}

/// Terminate a process tree after a bounded grace period (SPEC §9.3).
async fn terminate(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Signal the whole process group, so children of the child die too.
        unsafe {
            libc_kill(-(pid as i32), 15); // SIGTERM
        }
        // Bounded grace period before the kill.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        unsafe {
            libc_kill(-(pid as i32), 9); // SIGKILL
        }
    }
    #[cfg(not(unix))]
    let _ = pid;

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid, sig);
    }
}

/// Write an output file and describe it; the caller commits the record.
async fn write_artifact(
    store: &Store,
    run_id: &RunId,
    task_id: &TaskId,
    name: &str,
    bytes: &[u8],
    media_type: &str,
) -> Result<Artifact> {
    let path = store
        .artifact_dir(run_id)
        .join(format!("exec-{task_id}"))
        .join(name);
    write_atomic(&path, bytes).await?;

    Ok(Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        media_type: media_type.to_string(),
        content_hash: ContentHash::of_bytes(bytes),
        size_bytes: bytes.len() as u64,
        storage_path: store.storable_path(&path),
        produced_by: Some(task_id.clone()),
        access: AccessClass::Open,
        label: Some(name.to_string()),
        supersedes: None,
        superseded_reason: None,
        created_at: OffsetDateTime::now_utc(),
    })
}
