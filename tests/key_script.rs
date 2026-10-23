use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn run_script(home: &std::path::Path, name: &str, input: &str) -> std::process::Output {
    let mut child = Command::new("bash")
        .arg(format!(
            "{}/scripts/set-swissai-api-key",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg(name)
        .env("HOME", home)
        .env_remove("URUK_API_KEY_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{input}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn script_adds_any_safe_account_name_with_private_permissions() {
    let home = tempfile::tempdir().unwrap();
    let output = run_script(home.path(), "Researcher-7", "sk-rc-test-only");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let directory = home.path().join(".config/swissai");
    let key_file = directory.join("researcher-7.key");
    assert_eq!(
        std::fs::read_to_string(&key_file).unwrap(),
        "sk-rc-test-only\n"
    );
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let output_text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output_text.contains("sk-rc-test-only"));
}

#[test]
fn invalid_key_does_not_overwrite_an_existing_account() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".config/swissai");
    std::fs::create_dir_all(&directory).unwrap();
    let key_file = directory.join("alex.key");
    std::fs::write(&key_file, "sk-rc-existing\n").unwrap();

    let output = run_script(home.path(), "alex", "invalid key");

    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(key_file).unwrap(),
        "sk-rc-existing\n"
    );
}
