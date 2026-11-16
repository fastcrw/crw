//! Executable fixtures for the golden CLI journeys documented in the README.
//!
//! Covers the two journeys that need no network and no installed browser:
//!   - Journey A: bare `crw <URL>` prints clean markdown to stdout.
//!   - Journey C (transitional scope): `crw setup --local --non-interactive`
//!     completes without a prompt and writes `config.toml`.
//!
//! Journey B (MCP host registration) is exercised in an isolated home by
//! `mcp/crw-mcp/test/install.test.js`. Journey D is exercised from a fresh
//! checkout by CI invoking the documented `make check-fast` entry point.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// The fixtures live under `src/commands/` rather than in the workspace
/// `tests/` tree because `crw smoke` embeds one with `include_str!` and a
/// published crate only ships what is inside the crate directory.
fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/fixtures")
        .join(name)
}

/// Serve `fixture` over plain HTTP to every connection accepted, so
/// `crw <url>` has something deterministic and offline to scrape.
///
/// ponytail: hand-rolled instead of pulling in an HTTP server crate — a real
/// GET request arrives in one TCP read on loopback, so a full HTTP parser
/// buys nothing for a fixture that only ever serves one canned response.
fn spawn_fixture_server(fixture: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    let addr = listener.local_addr().expect("fixture server local_addr");
    let body = std::fs::read_to_string(fixture).expect("read fixture file");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    format!("http://{addr}")
}

/// One-request managed-search fixture. It returns the Cloud response shape and
/// sends the raw request back to the test so routing, auth, and body are all
/// asserted at the binary boundary.
fn spawn_cloud_search_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind cloud search fixture");
    let addr = listener.local_addr().expect("fixture server local_addr");
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept cloud search request");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set fixture read timeout");
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).expect("read cloud request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let Some(headers_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= headers_end + 4 + content_length {
                break;
            }
        }

        request_tx
            .send(String::from_utf8(request).expect("request is utf8"))
            .expect("send captured request");

        let body = r#"{"success":true,"data":[{"url":"https://example.com/rust","title":"Rust tutorial","description":"Learn Rust","snippet":"Learn Rust","position":1,"score":0.9,"category":"general"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write cloud response");
    });

    (format!("http://{addr}"), request_rx)
}

fn spawn_rejecting_cloud_search_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind rejecting cloud fixture");
    let addr = listener.local_addr().expect("fixture server local_addr");

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept cloud search request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        let body = r#"{"success":false,"error":"invalid API key"}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write cloud rejection");
    });

    format!("http://{addr}")
}

fn spawn_remote_scrape_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind remote scrape fixture");
    let addr = listener.local_addr().expect("fixture server local_addr");
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept remote scrape request");
        let mut request = [0u8; 8192];
        let read = stream.read(&mut request).expect("read scrape request");
        request_tx
            .send(String::from_utf8_lossy(&request[..read]).into_owned())
            .expect("send captured scrape request");

        let body = r##"{"success":true,"data":{"markdown":"# Smoke OK"}}"##;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write scrape response");
    });

    (format!("http://{addr}"), request_rx)
}

/// Journey A: `crw <URL>` needs no config or API key and prints the page's
/// main content as clean markdown, dropping nav/footer/script chrome.
#[test]
fn journey_a_bare_scrape_prints_markdown_from_local_fixture() {
    let base_url = spawn_fixture_server(&fixture_path("simple.html"));
    let config_dir = tempfile::tempdir().expect("tempdir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .arg(&base_url)
        // Isolated config dir: a bare scrape must work with zero config, and
        // this keeps the test from touching (or depending on) the real
        // ~/.config/crw on the machine running it.
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello World"))
        .stdout(predicate::str::contains("Item 1"))
        .stdout(predicate::str::contains("Footer content").not());
}

/// The curl installer must leave a fresh user at a runnable scrape command,
/// not take control of their terminal by launching the optional setup wizard.
#[test]
fn installer_does_not_auto_launch_optional_setup() {
    let installer = include_str!("../../../install.sh");
    assert!(installer.contains("crw https://example.com"));
    assert!(installer.contains("Optional:  crw setup"));
    assert!(
        !installer.contains("setup </dev/tty"),
        "installer must not auto-launch the interactive setup wizard"
    );
}

/// Resolving "which tag is latest" through the GitHub REST API costs a second
/// request that is capped at 60/hour per IP, so on any shared address (CI, an
/// office NAT, a cloud VM) it is the tag lookup that fails, not the download.
/// GitHub's own `releases/latest/download/<asset>` redirect has no such cap.
#[test]
fn installer_resolves_latest_without_the_rate_limited_github_api() {
    let installer = include_str!("../../../install.sh");
    assert!(
        !installer.contains("api.github.com"),
        "installer must not call the rate-limited GitHub REST API"
    );
    assert!(
        installer.contains("releases/latest/download/"),
        "installer must download through GitHub's latest-release redirect"
    );
    // Pinning an exact tag still has to reach the plain per-tag asset path.
    assert!(installer.contains("releases/download/${VERSION}/${ASSET}"));
    assert!(installer.contains("CRW_VERSION"));
}

/// Journey C (Phase 1 transitional scope): `crw setup --local` must be able
/// to run non-interactively — completing without touching stdin, writing
/// `~/.config/crw/config.toml`, and exiting zero. With no AI tool in HOME
/// there is nothing to register, so no integration section is printed.
#[test]
fn journey_c_setup_local_non_interactive_writes_config_and_exits_zero() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    // Hard timeout: `--non-interactive` must finish well under a second
    // (no daemon, no network, no prompt). If a future regression makes it
    // shell out to something that can block, fail fast here instead of
    // wedging the whole machine's cargo-slot lock.
    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["setup", "--local", "--non-interactive", "--no-color"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        // An empty HOME: scripted local setup registers MCP with every
        // detected AI tool, and a test must never touch the developer's own.
        .env("HOME", config_dir.path())
        .env("USERPROFILE", config_dir.path())
        .timeout(Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("LLM").not())
        .stdout(predicate::str::contains("Shell Configuration").not())
        .stdout(predicate::str::contains("AI TOOL INTEGRATION").not())
        .stdout(predicate::str::contains("MCP installed").not())
        .stdout(predicate::str::contains("source ").not())
        .stdout(predicate::str::contains("Add these to your shell").not());

    assert!(
        config_dir.path().join("config.toml").exists(),
        "non-interactive local setup must still write config.toml"
    );
}

/// Choosing Local after Cloud is a real mode switch. Persisted Cloud routing
/// must not silently keep winning after Local setup reports success.
#[test]
fn local_setup_removes_previous_cloud_routing() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    let config_path = config_dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[client]
api_url = "https://api.fastcrw.com"
api_key = "fc-old-cloud"

[search]
search_backend_url = "http://old-search:8080"
"#,
    )
    .expect("write prior cloud config");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["setup", "--local", "--non-interactive", "--no-color"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        // An empty HOME: scripted local setup registers MCP with every
        // detected AI tool, and a test must never touch the developer's own.
        .env("HOME", config_dir.path())
        .env("USERPROFILE", config_dir.path())
        .env_remove("CRW_API_URL")
        .env_remove("CRW_API_KEY")
        .timeout(Duration::from_secs(30))
        .assert()
        .success();

    let config = std::fs::read_to_string(config_path).expect("read switched config");
    assert!(!config.contains("[client]"));
    assert!(!config.contains("api.fastcrw.com"));
    assert!(!config.contains("fc-old-cloud"));
    assert!(!config.contains("old-search"));
}

/// A legacy shell export has higher precedence than the new Local config and
/// cannot be changed in the parent process. Setup must make that visible
/// instead of claiming a silent, ineffective mode switch.
#[test]
fn local_setup_warns_when_cloud_env_still_overrides_it() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["setup", "--local", "--non-interactive", "--no-color"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        // An empty HOME: scripted local setup registers MCP with every
        // detected AI tool, and a test must never touch the developer's own.
        .env("HOME", config_dir.path())
        .env("USERPROFILE", config_dir.path())
        .env("CRW_API_URL", "https://api.fastcrw.com")
        .env("CRW_API_KEY", "fc-legacy-shell")
        .timeout(Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Environment variables override Local config",
        ))
        .stdout(predicate::str::contains("crw setup --reset-shell"));
}

/// A Cloud setup config must route `crw search` to the managed API, not to
/// the localhost search fallback.
#[test]
fn cloud_config_routes_search_to_managed_api() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    let (api_url, request_rx) = spawn_cloud_search_server();
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            r#"
[client]
api_url = "{api_url}"
api_key = "fc-journey-test"
"#
        ),
    )
    .expect("write cloud config");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["search", "rust tutorials", "--limit", "3"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env_remove("CRW_API_URL")
        .env_remove("CRW_API_KEY")
        .env_remove("CRW_SEARCH_BACKEND_URL")
        .env_remove("CRW_SEARXNG_URL")
        .timeout(Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust tutorial"))
        .stdout(predicate::str::contains("https://example.com/rust"));

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture cloud search request");
    assert!(request.starts_with("POST /v1/search HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer fc-journey-test")
    );
    assert!(request.contains(r#""query":"rust tutorials""#));
    assert!(request.contains(r#""limit":3"#));
}

/// A custom CRW API URL may point at the default unauthenticated self-hosted
/// server. Supplying the URL selects remote search, but must not manufacture a
/// Cloud-only API-key requirement or authorization header.
#[test]
fn self_hosted_search_does_not_require_an_api_key() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    let (api_url, request_rx) = spawn_cloud_search_server();

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["search", "rust tutorials"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env("CRW_API_URL", &api_url)
        .env_remove("CRW_API_KEY")
        .env_remove("CRW_SEARCH_BACKEND_URL")
        .env_remove("CRW_SEARXNG_URL")
        .timeout(Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust tutorial"));

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture self-hosted search request");
    assert!(request.starts_with("POST /v1/search HTTP/1.1"));
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "unauthenticated self-host mode must not send a bearer header"
    );
}

/// Cloud failures must stay in the Cloud recovery path. Recommending a local
/// sidecar after the user deliberately selected Cloud recreates the original
/// setup bug and sends them toward unrelated infrastructure.
#[test]
fn cloud_search_failure_never_prints_local_backend_instructions() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    let api_url = spawn_rejecting_cloud_search_server();
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            r#"
[client]
api_url = "{api_url}"
api_key = "fc-rejected-test"
"#
        ),
    )
    .expect("write cloud config");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["search", "rust tutorials"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env_remove("CRW_API_URL")
        .env_remove("CRW_API_KEY")
        .env_remove("CRW_SEARCH_BACKEND_URL")
        .env_remove("CRW_SEARXNG_URL")
        .timeout(Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(predicate::str::contains("search failed"))
        .stderr(predicate::str::contains("SearXNG").not())
        .stderr(predicate::str::contains("localhost:8080").not())
        .stderr(predicate::str::contains("crw setup --local").not())
        .stderr(predicate::str::contains("docker run").not());
}

/// `crw doctor` on a fresh/empty config directory: no `[client].api_url` and
/// no local renderer means an unambiguous `local` resolution, so it must
/// exit deterministically (0 when every check passes, 1 if a real-machine
/// check like the listen-port probe happens to fail) — never 2, and its
/// `--json` output must always parse.
#[test]
fn doctor_fresh_config_dir_exits_deterministically_and_json_parses() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    let output = Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["doctor", "--json"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .output()
        .expect("run crw doctor");

    let code = output.status.code().expect("doctor exits with a code");
    assert!(
        code == 0 || code == 1,
        "fresh config dir must resolve to an unambiguous target (exit 0 or 1), got {code}"
    );

    let stdout = String::from_utf8(output.stdout).expect("doctor --json is valid utf8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("doctor --json output must parse as JSON");
    assert_eq!(parsed["target"], "local");
    assert!(
        parsed["checks"].as_array().is_some_and(|c| !c.is_empty()),
        "doctor --json must report at least one check"
    );
}

/// The public CLI/MCP environment names must resolve through the same config
/// path doctor and smoke use; otherwise Cloud users get local SearXNG advice.
#[test]
fn doctor_honors_public_remote_env_aliases() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    let output = Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["doctor", "--json"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env("CRW_API_URL", "http://127.0.0.1:1")
        .env_remove("CRW_API_KEY")
        .output()
        .expect("run crw doctor");

    let code = output.status.code().expect("doctor exits with a code");
    assert!(code == 0 || code == 1);
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json output must parse");
    assert_eq!(parsed["target"], "cloud");
    assert!(
        parsed["checks"]
            .as_array()
            .is_some_and(|checks| checks.iter().any(|check| check["id"] == "remote.api-key"))
    );
}

/// A config that sets both a cloud API and a local renderer is ambiguous —
/// `crw doctor` cannot guess which one the caller means, so it must refuse
/// with exit code 2 rather than silently picking one.
#[test]
fn doctor_ambiguous_target_exits_2() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        r#"
[client]
api_url = "https://api.fastcrw.com"
api_key = "crw_live_test"

[renderer.chrome]
ws_url = "ws://127.0.0.1:9222"
"#,
    )
    .expect("write ambiguous config.toml");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["doctor"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .assert()
        .code(2);
}

/// Bare `crw smoke` is `--offline --surface cli`: it must scrape only the
/// fixture compiled into the binary, touch no network, and exit 0.
#[test]
fn bare_smoke_runs_offline_and_exits_zero() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .arg("smoke")
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("smoke.cli.scrape"));
}

#[test]
fn smoke_rejects_malformed_config_instead_of_testing_defaults() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "[client\napi_url = '",
    )
    .expect("write malformed config");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["smoke", "--json"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env_remove("CRW_API_URL")
        .env_remove("CRW_API_KEY")
        .assert()
        .code(2)
        .stdout(predicate::str::contains("config failed to parse"));
}

#[test]
fn live_server_smoke_exercises_the_requested_url() {
    let config_dir = tempfile::tempdir().expect("tempdir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args([
            "smoke",
            "--live",
            "http://127.0.0.1:1",
            "--surface",
            "server",
            "--json",
        ])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env_remove("CRW_API_URL")
        .env_remove("CRW_API_KEY")
        .timeout(Duration::from_secs(15))
        .assert()
        .failure()
        .stdout(predicate::str::contains("smoke.server.scrape"))
        .stdout(predicate::str::contains("127.0.0.1:1"));
}

#[test]
fn live_mcp_smoke_calls_crw_scrape() {
    let config_dir = tempfile::tempdir().expect("tempdir");
    let (api_url, request_rx) = spawn_remote_scrape_server();
    let target_url = "https://example.com";

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["smoke", "--live", target_url, "--surface", "mcp", "--json"])
        .env("CRW_USER_CONFIG_DIR", config_dir.path())
        .env("CRW_API_URL", api_url)
        .env_remove("CRW_API_KEY")
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .stdout(predicate::str::contains("smoke.mcp.scrape"));

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture MCP proxy scrape request");
    assert!(request.starts_with("POST /v1/scrape HTTP/1.1"));
    assert!(request.contains(r#""url":"https://example.com""#));
}

/// Scripted local setup registers MCP with every detected AI tool, the same as
/// `--api-key`. npx is kept off PATH so the test stays offline: the installer
/// cannot start, which setup reports as non-fatal with the manual command.
#[test]
fn local_non_interactive_setup_registers_detected_agents() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    let empty_path = tempfile::tempdir().expect("empty PATH dir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args(["setup", "--local", "--non-interactive", "--no-color"])
        .env("CRW_USER_CONFIG_DIR", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", empty_path.path())
        .timeout(Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("AI TOOL INTEGRATION"))
        .stdout(predicate::str::contains("Detected: Codex"))
        .stdout(predicate::str::contains("install --from-config --codex"));
}

/// `--no-agents` keeps scripted local setup away from AI tool configs.
#[test]
fn local_non_interactive_setup_honours_no_agents() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    let empty_path = tempfile::tempdir().expect("empty PATH dir");

    Command::cargo_bin("crw")
        .expect("crw binary")
        .args([
            "setup",
            "--local",
            "--non-interactive",
            "--no-agents",
            "--no-color",
        ])
        .env("CRW_USER_CONFIG_DIR", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", empty_path.path())
        .timeout(Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("AI TOOL INTEGRATION").not());
}
