//! Acceptance tests for the web-only command surface.
//!
//! The browser is the only frontend: `uruk serve` is the sole command the
//! binary accepts. Research operations (starting, steering, stopping runs,
//! reports, passage search) live in the web API (`src/web`, docs/WEB.md).
//! These tests run the compiled binary itself, so they pin the contract a
//! user actually sees, not an internal parser type.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn uruk() -> Command {
    Command::new(env!("CARGO_BIN_EXE_uruk"))
}

/// Every command surface the web-only product removed. The names that never
/// existed (`list`, `inspect`, `export`, `search`, `passage`) are pinned
/// alongside the removed ones so none of them can quietly appear later.
const REMOVED_COMMANDS: &[&str] = &[
    "run", "status", "list", "inspect", "stop", "resume", "revise", "feedback", "approve", "deny",
    "export", "search", "passage", "passages", "report", "prompts",
];

#[test]
fn help_offers_serve_and_no_removed_command() {
    let out = uruk().arg("--help").output().expect("run uruk --help");
    assert!(out.status.success(), "--help must succeed");
    let help = String::from_utf8(out.stdout).expect("utf-8 help text");

    assert!(help.contains("serve"), "help must offer serve:\n{help}");
    for removed in REMOVED_COMMANDS {
        // A removed command must not be offered as a subcommand. `serve`'s
        // own description may mention nouns like "serve the web API", so
        // match the indented subcommand-list form clap prints.
        let as_subcommand = format!("\n  {removed}");
        assert!(
            !help.contains(&as_subcommand),
            "help must not offer `{removed}`:\n{help}"
        );
    }
}

#[test]
fn removed_commands_fail_to_parse_and_touch_no_state() {
    for removed in REMOVED_COMMANDS {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = uruk()
            .args(["--project", dir.path().to_str().expect("utf-8 path")])
            .arg(removed)
            .output()
            .unwrap_or_else(|e| panic!("spawn uruk {removed}: {e}"));

        assert_eq!(
            out.status.code(),
            Some(2),
            "`uruk {removed}` must fail at parse time (clap exit code 2)"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unrecognized subcommand"),
            "`uruk {removed}` must be unrecognized, got:\n{stderr}"
        );
        assert!(
            !dir.path().join(".uruk").exists(),
            "`uruk {removed}` must not create project state"
        );
    }
}

#[test]
fn no_command_is_a_usage_error() {
    let out = uruk().output().expect("run uruk");
    assert_eq!(out.status.code(), Some(2), "bare `uruk` must print usage");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Usage"), "expected usage text:\n{stderr}");
}

#[test]
fn serve_rejects_a_malformed_bind_address() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = uruk()
        .args(["--project", dir.path().to_str().expect("utf-8 path")])
        .args(["serve", "--bind", "not-an-address"])
        .output()
        .expect("run uruk serve");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a bad bind address is a runtime validation error, not a parse error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("bad --bind address"),
        "expected the bind validation message, got:\n{stderr}"
    );
}

/// Kill the server on drop so a failing assertion never leaks the process.
struct ServeGuard(Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serve_starts_and_answers_health() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Reserve a free port, then hand it to the server. The tiny window
    // between drop and bind is acceptable for a local test.
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("probe a free port")
        .local_addr()
        .expect("probe addr")
        .port();
    let bind = format!("127.0.0.1:{port}");

    let child = uruk()
        .args(["--project", dir.path().to_str().expect("utf-8 path")])
        .args(["serve", "--bind", &bind])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn uruk serve");
    let _guard = ServeGuard(child);

    let response =
        get_health(&bind, Duration::from_secs(30)).expect("serve must answer /api/health");
    assert!(
        response.contains("200 OK"),
        "expected HTTP 200, got:\n{response}"
    );
    assert!(
        response.contains("\"service\":\"uruk\""),
        "expected the uruk health body, got:\n{response}"
    );
}

/// Poll `GET /api/health` until the server answers or the deadline passes.
fn get_health(addr: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let request =
                format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok() && !response.is_empty() {
                    return Some(response);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}
