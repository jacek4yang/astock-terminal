//! Deterministic CLI tests for the offline, read-only inspection commands.
//!
//! These tests spawn the real `astock` binary against a throwaway data root and
//! never reach a model provider or a market upstream, so they consume no paid
//! API quota and need no credential. `stdin` is closed for every run, which
//! turns any attempt to prompt for a secret into a visible failure instead of a
//! hang.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// Run `astock` with the given arguments against an isolated environment.
///
/// Isolation uses the explicit `--data-dir` override rather than environment
/// variables. `HOME` and the XDG variables are enough on Linux and macOS, but
/// `directories` resolves the Windows data root through Win32 known-folder APIs
/// that ignore the environment entirely. Relying on env vars therefore looked
/// isolated on Linux while every Windows test shared the real user data
/// directory, which produced `database is locked` and schema conflicts as the
/// tests ran in parallel — and meant the suite was mutating real user state.
fn run(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_astock"));
    command
        .arg("--data-dir")
        .arg(root)
        .args(args)
        .env("HOME", root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("ASTOCK_DATA_DIR")
        .env_remove("ASTOCK_BUILD_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().expect("spawn the astock binary")
}

fn stdout_of(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout is UTF-8")
}

fn stderr_of(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("stderr is UTF-8")
}

/// Non-TTY stdout must stay a clean machine-readable stream.
fn assert_no_ansi(stream: &str, label: &str) {
    assert!(
        !stream.contains('\u{1b}'),
        "{label} emitted an ANSI escape sequence on a non-TTY stdout"
    );
}

#[test]
fn sources_json_is_a_bounded_array_on_an_empty_data_root() {
    let root = TempDir::new().expect("create a temporary data root");
    let output = run(root.path(), &["sources", "--json"]);

    assert!(
        output.status.success(),
        "sources --json exited with {:?}; stderr: {}",
        output.status.code(),
        stderr_of(&output)
    );

    let stdout = stdout_of(&output);
    assert_no_ansi(stdout, "sources --json");

    let parsed: serde_json::Value =
        serde_json::from_str(stdout).expect("sources --json emits exactly one JSON document");
    let rows = parsed
        .as_array()
        .expect("the source page is a JSON array, which the plain renderer also relies on");
    assert!(
        rows.is_empty(),
        "a fresh data root has no versioned source documents, found {}",
        rows.len()
    );
}

#[test]
fn cache_json_reports_every_counter_the_plain_renderer_reads() {
    let root = TempDir::new().expect("create a temporary data root");
    let output = run(root.path(), &["cache", "--json"]);

    assert!(
        output.status.success(),
        "cache --json exited with {:?}; stderr: {}",
        output.status.code(),
        stderr_of(&output)
    );

    let stdout = stdout_of(&output);
    assert_no_ansi(stdout, "cache --json");

    let stats: serde_json::Value =
        serde_json::from_str(stdout).expect("cache --json emits exactly one JSON document");
    let object = stats
        .as_object()
        .expect("cache statistics are a JSON object");

    // Every byte counter the plain renderer formats must be an unsigned
    // integer, otherwise that path silently degrades to "unknown".
    for key in [
        "total_bytes",
        "sqlite_bytes",
        "kline_parquet_bytes",
        "tool_cache_bytes",
        "chat_bytes",
    ] {
        let value = object
            .get(key)
            .unwrap_or_else(|| panic!("cache statistics expose `{key}`"));
        assert!(
            value.as_u64().is_some(),
            "`{key}` must be an unsigned byte count, found {value}"
        );
    }

    // Free space is deliberately optional: `astock_storage::disk_free_bytes`
    // only queries the platform on Windows and documents `None` as "unknown"
    // elsewhere. The key must still be present so the CLI can render it.
    let disk_free = object
        .get("disk_free_bytes")
        .expect("cache statistics expose `disk_free_bytes`");
    assert!(
        disk_free.is_null() || disk_free.as_u64().is_some(),
        "`disk_free_bytes` is either a byte count or null for unknown, found {disk_free}"
    );
}

#[test]
fn inspection_commands_need_no_credential_and_never_prompt() {
    let root = TempDir::new().expect("create a temporary data root");

    // stdin is closed by `run`, so a hidden-prompt attempt cannot succeed.
    // Both commands must still complete, proving they stay on the read-only
    // Engine path rather than the provider path.
    for args in [
        ["sources", "--json"].as_slice(),
        ["cache", "--json"].as_slice(),
    ] {
        let output = run(root.path(), args);
        assert!(
            output.status.success(),
            "{args:?} must succeed without a credential, exited with {:?}; stderr: {}",
            output.status.code(),
            stderr_of(&output)
        );
        let stderr = stderr_of(&output);
        for forbidden in ["API key", "api key", "password", "MiniMax key", "JoinQuant"] {
            assert!(
                !stderr.contains(forbidden),
                "{args:?} must not ask for a credential, stderr mentioned `{forbidden}`"
            );
        }
    }
}

#[test]
fn compact_without_a_durable_session_fails_as_an_explicit_configuration_error() {
    let root = TempDir::new().expect("create a temporary data root");
    let output = run(root.path(), &["compact"]);

    // `RuntimeError::Configuration` maps to exit code 2. A panic would surface
    // as 101 and a silent success as 0; both are regressions.
    assert_eq!(
        output.status.code(),
        Some(2),
        "compact on an empty data root must exit with the configuration code; stderr: {}",
        stderr_of(&output)
    );

    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("there is no durable session to compact"),
        "compact must explain why it refused, stderr was: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "compact must fail as a typed error, not a panic: {stderr}"
    );
    assert!(
        stdout_of(&output).trim().is_empty(),
        "a refused compact must not emit a result document on stdout"
    );
}

/// The interactive adapter must start and answer control intents before any
/// model credential exists.
///
/// Requiring a MiniMax key just to type `/help` contradicts the product's
/// launch-and-type promise, so the provider is attached lazily on the first
/// research request. This test pins that: it drives the loop through a
/// pseudo-terminal with no credential available and expects control output, not
/// a credential prompt.
///
/// The test is skipped rather than failed when no usable `script` is present,
/// because a missing pty helper is an environment gap, not a product regression.
/// Windows has no `script` at all, and its interactive path is covered by the
/// non-interactive tests plus the runtime's own intent suite.
#[test]
fn interactive_control_intents_work_without_a_model_credential() {
    let Some(script) = which_script() else {
        eprintln!("skipping: no `script` binary available to allocate a pseudo-terminal");
        return;
    };
    let root = TempDir::new().expect("create a temporary data root");
    let input = root.path().join("input.txt");
    // Natural language only: no slash command is used anywhere here.
    std::fs::write(&input, "你有哪些工具\n现在什么状态\n退出\n").expect("write driver input");

    let binary = env!("CARGO_BIN_EXE_astock");
    let data_dir = root.path().display().to_string();
    let mut command = Command::new(script);
    if cfg!(target_os = "macos") {
        // BSD script: `script -q <logfile> <command> [args...]`. It does not
        // accept GNU's `-c`, which is why the GNU form failed on macOS.
        command
            .arg("-q")
            .arg("/dev/null")
            .arg(binary)
            .arg("--data-dir")
            .arg(&data_dir)
            .arg("chat");
    } else {
        // GNU script: the command is one string passed to `-c`.
        command.args([
            "-qec",
            &format!("{binary} --data-dir {data_dir} chat"),
            "/dev/null",
        ]);
    }
    let output = command
        .env("HOME", root.path())
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env_remove("ASTOCK_DATA_DIR")
        .stdin(std::fs::File::open(&input).expect("open driver input"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the interactive adapter under a pseudo-terminal");

    let combined = format!("{}{}", stdout_of(&output), stderr_of(&output));
    assert!(
        !combined.contains("MiniMax API key") && !combined.contains("API key:"),
        "control intents must not trigger a credential prompt, got: {combined}"
    );
    assert!(
        combined.contains("get_quote"),
        "`你有哪些工具` must list the bounded tool registry, got: {combined}"
    );
    assert!(
        combined.contains("phase=idle"),
        "`现在什么状态` must report durable task status, got: {combined}"
    );
}

fn which_script() -> Option<&'static str> {
    ["/usr/bin/script", "/bin/script"]
        .into_iter()
        .find(|path| Path::new(path).exists())
}
