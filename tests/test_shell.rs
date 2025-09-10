use std::path::{Path, PathBuf};
use std::sync::Arc;

// Prefer importing the crate under test by its package name.
// In Cargo integration tests, `crate` refers to the test crate, so use the package name.
// If the package is named differently, replace `crate_under_test` with the actual package name.
// To help CI, we also try the implicit `::` path via `use` fallback behind cfg_attr.
#[allow(unused_imports)]
use crate_under_test as sut;

use serde_json as _; // rely on serde_json if present; otherwise we avoid direct usage
use uuid::Uuid;

fn deserialize_shell_posix(shell_path: &str, rc_path: &str) -> sut::Shell {
    // Deserialize via serde_json to bypass private field visibility for PosixShell.
    // shell_snapshot is skipped by serde, which yields None (desired for these tests).
    let json = format\!(r#"{{
        "Posix": {{
            "shell_path": "{}",
            "rc_path": "{}"
        }}
    }}"#, shell_path, rc_path);
    serde_json::from_str::<sut::Shell>(&json).expect("deserialize Shell::Posix")
}

fn deserialize_shell_powershell(exe: &str, bash_fallback: Option<&str>) -> sut::Shell {
    let fallback_json = match bash_fallback {
        Some(p) => format\!(r#""{}""#, p.replace('\\', "\\\\") ),
        None => "null".to_string(),
    };
    let json = format\!(r#"{{
        "PowerShell": {{
            "exe": "{}",
            "bash_exe_fallback": {}
        }}
    }}"#, exe, fallback_json);
    serde_json::from_str::<sut::Shell>(&json).expect("deserialize Shell::PowerShell")
}

#[test]
fn unknown_shell_has_no_name_and_no_invocation() {
    let s = sut::Shell::Unknown;
    assert_eq\!(s.name(), None, "Unknown shell should return no name");
    let out = s.format_default_shell_invocation(vec\!["echo".into()]);
    assert\!(out.is_none(), "Unknown shell should not format invocations");
}

#[test]
fn posix_shell_formats_simple_command_with_rc_source_and_lc() {
    // Use spaces in rc_path to ensure quoting occurs.
    let shell_path = "/bin/zsh";
    let rc_path = "/home/test user/.zshrc";
    let s = deserialize_shell_posix(shell_path, rc_path);

    let out = s.format_default_shell_invocation(vec\!["echo".into(), "hello".into(), "world".into()])
        .expect("should produce invocation");
    assert_eq\!(out[0], shell_path);
    assert_eq\!(out[1], "-lc", "without snapshot, should use login shell (-lc)");

    // rc_command should source rc if present check succeeds and then run our command in subshell.
    let rc_cmd = &out[2];
    // Check structure and important substrings instead of exact quoting.
    assert\!(rc_cmd.contains("[ -f "), "should test rc presence");
    assert\!(rc_cmd.contains(" ] && . "), "should source rc when present");
    assert\!(rc_cmd.contains("; (echo hello world)"), "should run joined command in subshell");
}

#[test]
fn posix_shell_prefers_script_when_input_is_bash_dash_lc() {
    let shell_path = "/bin/bash";
    let rc_path = "/home/test/.bashrc";
    let s = deserialize_shell_posix(shell_path, rc_path);

    let out = s.format_default_shell_invocation(vec\!["bash".into(), "-lc".into(), "printf ok".into()])
        .expect("should produce invocation");
    assert_eq\!(out[0], shell_path);
    assert_eq\!(out[1], "-lc", "no snapshot -> -lc");
    let rc_cmd = &out[2];
    assert\!(rc_cmd.ends_with("(printf ok)"), "should use provided script as-is without re-quoting");
}

#[test]
fn powershell_with_bash_fallback_runs_bash_script_via_fallback() {
    let bash_fallback = if cfg\!(target_os = "windows") {
        r"C:\Windows\System32\bash.exe"
    } else {
        "/usr/bin/bash.exe"
    };
    let s = deserialize_shell_powershell("pwsh.exe", Some(bash_fallback));

    let out = s.format_default_shell_invocation(vec\!["bash".into(), "-lc".into(), "echo hi".into()])
        .expect("should produce invocation");

    assert_eq\!(out[0], bash_fallback, "should use bash fallback when bash -lc is detected");
    assert_eq\!(out[1], "-lc");
    assert_eq\!(out[2], "echo hi");
}

#[test]
fn powershell_without_bash_fallback_runs_script_under_powershell() {
    let s = deserialize_shell_powershell("pwsh.exe", None);

    let out = s.format_default_shell_invocation(vec\!["bash".into(), "-lc".into(), "echo hi".into()])
        .expect("should produce invocation");

    assert_eq\!(out[0], "pwsh.exe");
    assert_eq\!(out[1], "-NoProfile");
    assert_eq\!(out[2], "-Command");
    assert_eq\!(out[3], "echo hi");
}

#[test]
fn powershell_wraps_non_ps_command_into_ps_command_with_escaping() {
    let s = deserialize_shell_powershell("pwsh.exe", None);

    let out = s.format_default_shell_invocation(vec\![
        "Write-Host".into(),
        "line1\nline2".into(),
        "carriage\rreturn".into(),
    ]).expect("should produce invocation");

    assert_eq\!(out[0], "pwsh.exe");
    assert_eq\!(out[1], "-NoProfile");
    assert_eq\!(out[2], "-Command");

    // The joined argument should have backtick-escaped newlines and carriage returns.
    let joined = &out[3];
    assert\!(joined.contains("`n"), "should escape newline to `n");
    assert\!(joined.contains("`r"), "should escape carriage return to `r");
    assert\!(joined.contains("Write-Host"), "should include original command");
}

#[test]
fn powershell_preserves_already_ps_command() {
    let s = deserialize_shell_powershell("powershell.exe", None);
    let cmd = vec\![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-Command".into(),
        "Get-Process".into(),
    ];
    let out = s.format_default_shell_invocation(cmd.clone()).expect("should return as-is");
    assert_eq\!(out, cmd, "should pass through when already PowerShell command");
}

#[test]
fn posix_name_is_filename_of_shell_path() {
    let s = deserialize_shell_posix("/usr/local/bin/zsh", "/home/test/.zshrc");
    assert_eq\!(s.name(), Some("zsh".to_string()));
}

#[test]
fn powershell_name_is_exe_field() {
    let s = deserialize_shell_powershell("pwsh.exe", None);
    assert_eq\!(s.name(), Some("pwsh.exe".to_string()));
}

#[test]
fn shellsnapshot_drop_deletes_file() {
    use std::fs;

    // Create a temporary file path.
    let dir = std::env::temp_dir();
    let test_file = dir.join(format\!("codex_test_delete_{}.tmp", Uuid::new_v4()));
    fs::write(&test_file, b"data").expect("create temp file");

    assert\!(test_file.exists(), "precondition: temp file exists");

    {
        // Create snapshot pointing at the temp file; when dropped, it should delete the file.
        let _snapshot = sut::ShellSnapshot::new(test_file.clone());
        // Drop at the end of this scope.
    }

    // After drop, the file should be gone (delete_shell_snapshot ignores errors but should remove existing file).
    assert\!(
        \!test_file.exists(),
        "ShellSnapshot Drop should remove the file at captured path"
    );
}