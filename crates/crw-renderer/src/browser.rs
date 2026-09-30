//! Auto-detect and spawn a headless browser for JS rendering in embedded mode.
//!
//! Priority order:
//! 1. LightPanda binary (PATH or `~/.crw/lightpanda`, auto-downloaded if missing)
//! 2. Chrome/Chromium binary (heavier but widely available)
//! 3. LightPanda Docker container (last resort, requires Docker daemon)
//!
//! The spawned process/container is automatically cleaned up on drop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Process-group IDs of every browser we spawned. Each native browser is
/// spawned with `process_group(0)`, making it its own group leader, so the
/// pgid equals the child PID. Group-killing the pgid reaps the browser plus
/// every grandchild (Chrome zygote/renderers, LightPanda helpers) that a
/// direct-PID `start_kill()` would miss (rust-lang/rust#115241).
///
/// This registry is the only thing robust to `process::exit`/signal — the
/// dominant leak cause — because it does not depend on `Drop` running.
#[cfg(unix)]
static BROWSER_PGIDS: LazyLock<Mutex<HashSet<i32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Per-launch Chrome profile directories we created. Chrome without
/// `--user-data-dir` drops a full profile (~15 MB) into the OS temp dir that
/// nothing ever removes, so we own the directory and delete it ourselves.
/// Like the pgid registry this survives `process::exit`, which skips `Drop`.
static BROWSER_PROFILES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn lock_profiles() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    BROWSER_PROFILES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lock the registry, recovering from a poisoned mutex. A panic in one
/// teardown path must not cascade-abort the others.
#[cfg(unix)]
fn lock_pgids() -> std::sync::MutexGuard<'static, HashSet<i32>> {
    BROWSER_PGIDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Register a freshly-spawned child's process group. Returns the pgid to
/// store on the guard, or `None` if the child already exited (no panic).
#[cfg(unix)]
fn register_child(child: &Child) -> Option<i32> {
    let pgid = child.id()? as i32;
    lock_pgids().insert(pgid);
    tracing::debug!(pgid, "registered browser process group");
    Some(pgid)
}

/// Windows has no process groups. Put the browser in a kill-on-close job so
/// the whole tree dies with us, even when we are terminated without running
/// any teardown code.
#[cfg(windows)]
fn register_child(child: &Child) -> Option<i32> {
    if let Some(handle) = child.raw_handle() {
        job::assign(handle);
    }
    None
}

/// Drop a pgid from the registry the moment its group leader is reaped, so
/// the set does not hold stale pgids across a normal browser lifetime
/// (PID-reuse mitigation).
#[cfg(unix)]
fn deregister_pgid(pgid: i32) {
    lock_pgids().remove(&pgid);
    tracing::debug!(pgid, "deregistered browser process group");
}

/// Kill every browser we spawned and delete their profile directories.
/// Idempotent and safe to call from a signal/teardown path. This, not `Drop`,
/// is what runs on the `process::exit` paths.
pub fn kill_all_browsers() {
    #[cfg(unix)]
    kill_all_process_groups();
    #[cfg(windows)]
    job::terminate();
    remove_registered_profiles();
}

/// SIGKILL every still-registered browser process group. Drains under the
/// lock then kills lock-free so a re-entrant signal cannot deadlock on the
/// registry mutex.
#[cfg(unix)]
fn kill_all_process_groups() {
    let pgids: Vec<i32> = {
        let mut set = lock_pgids();
        set.drain().collect()
    };
    let total = pgids.len();
    let mut killed = 0usize;
    let mut already_gone = 0usize;
    for pgid in pgids {
        // SAFETY: killpg is async-signal-safe. The residual race (leader
        // reaped + pgid reused between drain and killpg) is a documented,
        // accepted rare trade-off (see plan Open Questions).
        if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
            killed += 1;
        } else {
            already_gone += 1;
        }
    }
    if total > 0 {
        tracing::info!(
            registered = total,
            killed,
            already_gone,
            "kill_all_browsers: reaped browser process groups"
        );
    }
}

fn remove_registered_profiles() {
    let dirs: Vec<PathBuf> = lock_profiles().drain().collect();
    for dir in dirs {
        // The job/group kill above is asynchronous: give the tree a moment
        // to release its file handles (Windows refuses to delete open files).
        for attempt in 0..5 {
            if remove_profile_dir(&dir) {
                break;
            }
            if attempt < 4 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Windows job holding every browser we spawn. `KILL_ON_JOB_CLOSE` makes the
/// OS kill the whole tree when our last handle to the job closes, which
/// includes hard termination of this process.
#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// The job handle as an integer so the static is `Send + Sync`. Never
    /// closed on purpose: it lives for the whole process.
    static JOB: OnceLock<Option<usize>> = OnceLock::new();

    fn handle() -> Option<*mut c_void> {
        let job = JOB.get_or_init(|| {
            // SAFETY: plain FFI; the info struct is zeroed then filled in.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                (ok != 0).then_some(job as usize)
            }
        });
        job.map(|j| j as *mut c_void)
    }

    pub fn assign(process: *mut c_void) {
        let Some(job) = handle() else {
            tracing::warn!("could not create browser job object; browser may outlive us");
            return;
        };
        // SAFETY: both handles are valid; the process handle is owned by the caller.
        if unsafe { AssignProcessToJobObject(job, process) } == 0 {
            tracing::warn!("could not assign browser to job object");
        }
    }

    pub fn terminate() {
        if let Some(job) = JOB.get().copied().flatten() {
            // SAFETY: the job handle is valid for the process lifetime.
            unsafe { TerminateJobObject(job as *mut c_void, 1) };
        }
    }
}

/// Is `pid` a live process? Used to tell a running sibling's profile from a
/// dead process's leftovers.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks existence and permission.
        let exists = unsafe { libc::kill(pid, 0) } == 0;
        exists || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain FFI; the handle is closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let mut code = 0u32;
            let alive = GetExitCodeProcess(handle, &mut code) == 0 || code == 259; // STILL_ACTIVE
            CloseHandle(handle);
            alive
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Where per-launch Chrome profiles live: an app-owned cache directory, not
/// the shared OS temp dir, so a profile (cookies, session state) is not
/// world-readable and leftovers are attributable to us.
fn profile_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("crw")
        .join("chrome-profiles")
}

/// Create a fresh private profile directory named `<pid>-<nanos>` under `root`
/// and register it for teardown. The pid prefix is what lets a later run tell
/// leftovers from a live sibling. Sweeps dead processes' leftovers first.
fn create_profile_dir(root: &Path) -> Option<PathBuf> {
    sweep_stale_profiles(root);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("{}-{nanos}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&dir)
        .map_err(|e| tracing::warn!("Failed to create Chrome profile dir: {e}"))
        .ok()?;
    lock_profiles().insert(dir.clone());
    Some(dir)
}

/// Delete profile directories under `root` whose owning process is gone
/// (assumes one pid namespace per cache dir; containers sharing one mounted
/// cache dir could sweep each other)
/// (crash, SIGKILL, or a terminated MCP host). Best effort: a directory a
/// surviving browser still locks is retried on the next launch.
fn sweep_stale_profiles(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let owner = entry
            .file_name()
            .to_str()
            .and_then(|name| name.split('-').next()?.parse::<u32>().ok());
        // Never touch a directory this process still owns. An unregistered
        // one with our own pid is a previous run's (container restarts reuse
        // pid 1), so it is swept like any dead owner's.
        if let Some(pid) = owner
            && !lock_profiles().contains(&entry.path())
            && (pid == std::process::id() || !pid_alive(pid))
        {
            remove_profile_dir(&entry.path());
        }
    }
}

/// Best-effort delete. Returns whether the directory is gone.
fn remove_profile_dir(dir: &Path) -> bool {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), "could not remove Chrome profile dir: {e}");
            false
        }
    }
}

/// A managed browser process or Docker container.
/// Automatically cleaned up when dropped.
pub struct ManagedBrowser {
    kind: BrowserKind,
}

enum BrowserKind {
    /// A native process (LightPanda binary or Chrome). `pgid` is the
    /// process-group id registered in `BROWSER_PGIDS` (`None` if the child
    /// had already exited at spawn time, or on non-Unix).
    /// `profile` is the Chrome user-data-dir we created and must delete.
    Process {
        child: Child,
        pgid: Option<i32>,
        profile: Option<PathBuf>,
    },
    /// A Docker container, identified by its container ID.
    Docker(String),
}

impl Drop for ManagedBrowser {
    fn drop(&mut self) {
        match &mut self.kind {
            BrowserKind::Process {
                child,
                pgid,
                profile,
            } => {
                #[cfg(unix)]
                if let Some(pg) = *pgid {
                    // SAFETY: killpg is async-signal-safe. Group-kill first
                    // so Chrome zygote/renderers + LightPanda helpers die,
                    // not just the direct child PID.
                    unsafe { libc::killpg(pg, libc::SIGKILL) };
                    deregister_pgid(pg);
                }
                #[cfg(not(unix))]
                let _ = pgid;
                let _ = child.start_kill();
                // Exactly one non-blocking reap attempt — never block a
                // tokio worker (no loop, no sleep, never `wait()`). Full
                // zombie reaping for the rare long-lived-parent case is
                // offloaded to the teardown path; short-lived CLI runs are
                // reaped by the OS on process exit.
                let _ = child.try_wait();
                // Stay registered if removal fails (Windows: the browser tree
                // may still hold files open) so `kill_all_browsers` retries.
                if let Some(dir) = profile.take()
                    && remove_profile_dir(&dir)
                {
                    lock_profiles().remove(&dir);
                }
            }
            BrowserKind::Docker(container_id) => {
                // Best-effort stop + remove. Fire-and-forget.
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", container_id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        }
    }
}

/// Which renderer engine was spawned.
#[derive(Debug, Clone, Copy)]
pub enum RendererKind {
    LightPanda,
    Chrome,
}

/// Try to spawn a browser. Returns the managed handle + WS URL for CDP.
///
/// Tries in order: LightPanda native → Chrome native → LightPanda Docker.
pub async fn spawn_headless() -> Option<(ManagedBrowser, String)> {
    // 1. Try LightPanda native binary (PATH, ~/.crw/lightpanda, or auto-download).
    if let Some(result) = try_lightpanda_native().await {
        return Some(result);
    }

    // 2. Fallback to Chrome/Chromium native binary (widely available).
    if let Some(result) = try_chrome_native().await {
        return Some(result);
    }

    // 3. Last resort: LightPanda via Docker (requires Docker daemon).
    try_lightpanda_docker().await
}

/// Spawn all available browsers for a multi-renderer fallback chain.
///
/// Unlike `spawn_headless()` which returns the first browser found, this
/// function spawns every available browser so that `FallbackRenderer` can
/// try LightPanda first (fast, lightweight) and fall back to Chrome
/// (heavier but handles complex SPAs).
///
/// Docker is only tried if no native browser was found at all.
pub async fn spawn_all_headless() -> Vec<(ManagedBrowser, String, RendererKind)> {
    let mut browsers = Vec::new();

    // 1. Try LightPanda native (fast, lightweight).
    if let Some((guard, ws_url)) = try_lightpanda_native().await {
        browsers.push((guard, ws_url, RendererKind::LightPanda));
    }

    // 2. Also try Chrome/Chromium native (robust for complex SPAs).
    if let Some((guard, ws_url)) = try_chrome_native().await {
        browsers.push((guard, ws_url, RendererKind::Chrome));
    }

    // 3. Docker only if nothing native was found (last resort).
    if browsers.is_empty()
        && let Some((guard, ws_url)) = try_lightpanda_docker().await
    {
        browsers.push((guard, ws_url, RendererKind::LightPanda));
    }

    browsers
}

/// Detect the Chrome/Chromium executable the one-shot CLI runtime would
/// auto-spawn, without launching a process. Diagnostics use this to report
/// the same local capability the renderer will actually discover at request
/// time instead of looking only at configured remote CDP endpoints.
pub fn detect_local_chrome() -> Option<String> {
    find_chrome()
}

/// Detect an already-installed LightPanda executable without downloading or
/// launching it. Mirrors the runtime's PATH + managed-install lookup.
pub fn detect_local_lightpanda() -> Option<String> {
    if let Some(path) = find_in_path("lightpanda") {
        return Some(path);
    }
    let path = lightpanda_managed_path()?;
    (path.exists() && path.is_file()).then(|| path.to_string_lossy().to_string())
}

// --- LightPanda native ---

/// Find LightPanda binary: PATH → ~/.crw/lightpanda → auto-download.
async fn find_or_download_lightpanda() -> Option<String> {
    // 1. Check PATH.
    if let Some(bin) = find_in_path("lightpanda") {
        tracing::info!("Found LightPanda in PATH: {bin}");
        return Some(bin);
    }

    // 2. Check ~/.crw/lightpanda (our managed install location).
    let managed_path = lightpanda_managed_path()?;
    if managed_path.exists() {
        let path_str = managed_path.to_string_lossy().to_string();
        tracing::info!("Found managed LightPanda: {path_str}");
        return Some(path_str);
    }

    // 3. Auto-download from GitHub releases.
    let download_url = lightpanda_download_url()?;
    tracing::info!("Downloading LightPanda from {download_url}...");

    if let Some(parent) = managed_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("Failed to create ~/.crw directory: {e}");
        return None;
    }

    let output = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(managed_path.as_os_str())
        .arg(&download_url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?
        .wait_with_output()
        .await
        .ok()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("Failed to download LightPanda: {stderr}");
        // Clean up partial download.
        let _ = std::fs::remove_file(&managed_path);
        return None;
    }

    // Make executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&managed_path, std::fs::Permissions::from_mode(0o755))
        {
            tracing::warn!("Failed to chmod LightPanda binary: {e}");
            let _ = std::fs::remove_file(&managed_path);
            return None;
        }
    }

    let path_str = managed_path.to_string_lossy().to_string();
    tracing::info!("LightPanda downloaded to {path_str}");
    Some(path_str)
}

/// Get the managed install path: ~/.crw/lightpanda
fn lightpanda_managed_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".crw").join("lightpanda"))
}

/// Get the correct download URL for the current platform.
fn lightpanda_download_url() -> Option<String> {
    let base = "https://github.com/lightpanda-io/browser/releases/download/nightly";

    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(format!("{base}/lightpanda-aarch64-macos")),
        ("linux", "x86_64") => Some(format!("{base}/lightpanda-x86_64-linux")),
        ("linux", "aarch64") => Some(format!("{base}/lightpanda-aarch64-linux")),
        (os, arch) => {
            tracing::debug!("No LightPanda binary available for {os}/{arch}");
            None
        }
    }
}

/// Ranges [`crw_core::url_safety`] rejects that LightPanda's own
/// `--block-private-networks` group does not cover, so the browser we launch
/// enforces the same policy the rest of the pipeline does.
/// Omits `64:ff9b::/96`: [`crw_core::url_safety`] decodes that prefix and allows
/// a public embedded IPv4, because on an IPv6-only network with a DNS64/NAT64
/// resolver the whole v4 web resolves inside it, and a CIDR list cannot express
/// "carrying a private IPv4". `64:ff9b:1::/48` stays, RFC 8215 reserves it for
/// local use. `2002::/16` also stays even though `url_safety` decodes it too:
/// 6to4 is decommissioned (RFC 7526) so nothing real resolves there and the
/// stricter setting costs no recall, while this flag is the only control
/// covering worker targets, which never reach the CDP pump. ULA and v6
/// link-local are listed explicitly rather than assumed to be in LightPanda's
/// private group, and so are the IPv4 ranges that matter most (RFC1918,
/// loopback, link-local): this flag is the only control on the paths the CDP
/// pump cannot see, so it should not rest on an assumption about what upstream's
/// private group covers.
const LIGHTPANDA_EXTRA_BLOCK_CIDRS: &str = "0.0.0.0/8,10.0.0.0/8,127.0.0.0/8,169.254.0.0/16,172.16.0.0/12,192.168.0.0/16,100.64.0.0/10,224.0.0.0/4,240.0.0.0/4,\
192.0.0.0/24,192.0.2.0/24,198.18.0.0/15,198.51.100.0/24,203.0.113.0/24,\
fc00::/7,fe80::/10,fec0::/10,ff00::/8,::/96,64:ff9b:1::/48,2002::/16";

async fn try_lightpanda_native() -> Option<(ManagedBrowser, String)> {
    let bin = find_or_download_lightpanda().await?;

    // Find an available port for LightPanda.
    let port = find_available_port()?;
    let port_str = port.to_string();

    let mut cmd = Command::new(&bin);
    // Refuse private/internal destinations in the browser itself. This is the
    // only control that sees what the CDP interception pump cannot — websockets,
    // worker targets — and it runs on the resolved socket address, so it has no
    // time-of-check window. An older binary that does not know the flag exits,
    // the readiness poll below fails, and the ladder moves on.
    cmd.args([
        "serve",
        "--host",
        "127.0.0.1",
        "--port",
        &port_str,
        "--block-private-networks",
        "--block-cidrs",
        LIGHTPANDA_EXTRA_BLOCK_CIDRS,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    // Own process group: a group-kill reaps any LightPanda helper procs,
    // and detaching from crw's terminal group means Ctrl-C is delivered
    // by the teardown task, not twice. (Must ship with Phase 2 teardown.)
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| tracing::warn!("Failed to spawn LightPanda: {e}"))
        .ok()?;

    // Register the pgid BEFORE readiness polling — there is a real leak
    // window if Ctrl-C lands during the 5s poll. Build the guard now so a
    // poll failure drops it (→ killpg + deregister) instead of orphaning.
    let pgid = register_child(&child);
    let guard = ManagedBrowser {
        kind: BrowserKind::Process {
            child,
            pgid,
            profile: None,
        },
    };

    // LightPanda doesn't print a WS URL to stderr like Chrome does.
    // Poll /json/version until it's ready (up to 5 seconds).
    let ws_url = poll_cdp_endpoint(port, 5).await?; // guard drops on None
    tracing::info!("LightPanda CDP endpoint: {ws_url}");

    Some((guard, ws_url))
}

// --- LightPanda Docker ---

async fn try_lightpanda_docker() -> Option<(ManagedBrowser, String)> {
    // Check if Docker is available.
    if !command_exists("docker") {
        return None;
    }

    tracing::info!("Trying LightPanda via Docker...");

    // `docker run --rm -d -p 0:9222` → random host port mapped to 9222.
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-d",
            "-p",
            "0:9222",
            "lightpanda/browser:latest",
            // Overrides the image CMD, so the whole serve line has to be
            // repeated. Same reasoning as the native launch above.
            "/bin/lightpanda",
            "serve",
            "--host",
            "0.0.0.0",
            "--port",
            "9222",
            "--block-private-networks",
            "--block-cidrs",
            LIGHTPANDA_EXTRA_BLOCK_CIDRS,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?
        .wait_with_output()
        .await
        .ok()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!("LightPanda Docker failed: {stderr}");
        return None;
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if container_id.is_empty() {
        return None;
    }

    tracing::info!("LightPanda container started: {}", &container_id[..12]);

    // Get the mapped host port via `docker port`.
    let port = get_docker_mapped_port(&container_id, 9222).await?;

    // LightPanda needs a moment to start listening.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let ws_url = format!("ws://127.0.0.1:{port}/");
    tracing::info!("LightPanda Docker CDP endpoint: {ws_url}");

    Some((
        ManagedBrowser {
            kind: BrowserKind::Docker(container_id),
        },
        ws_url,
    ))
}

async fn get_docker_mapped_port(container_id: &str, container_port: u16) -> Option<u16> {
    let output = Command::new("docker")
        .args(["port", container_id, &container_port.to_string()])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Output format: "0.0.0.0:55000\n" or "0.0.0.0:55000\n:::55000\n"
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()?
        .rsplit(':')
        .next()?
        .trim()
        .parse()
        .ok()
}

// --- Chrome/Chromium native ---

/// Environment variables that pin the Chrome executable explicitly, in
/// precedence order. `CHROME_PATH` is the de-facto cross-tool convention.
const CHROME_PATH_VARS: &[&str] = &["CRW_CHROME_PATH", "CHROME_PATH"];

/// Absolute install locations, then bare names resolved against `PATH`.
/// Kept per-platform so a lookup never wastes a PATH scan on a path shape
/// that cannot exist on this OS.
#[cfg(target_os = "macos")]
const CHROME_CANDIDATES: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

#[cfg(windows)]
const CHROME_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files\Chromium\Application\chrome.exe",
    // Edge is Chromium-based and speaks CDP, and it ships with the OS — the
    // last-resort renderer on a machine with no Chrome install.
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    "chrome",
    "chromium",
    "msedge",
];

#[cfg(all(unix, not(target_os = "macos")))]
const CHROME_CANDIDATES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
];

fn find_chrome() -> Option<String> {
    // 1. Explicit override always wins.
    for var in CHROME_PATH_VARS {
        let Some(raw) = std::env::var_os(var) else {
            continue;
        };
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
        tracing::warn!("{var} is set but is not a file: {}", path.display());
    }

    // 2. Known install locations, then PATH.
    for candidate in CHROME_CANDIDATES {
        let path = std::path::Path::new(candidate);
        if path.is_absolute() {
            if path.is_file() {
                return Some((*candidate).to_string());
            }
        } else if let Some(found) = find_in_path(candidate) {
            return Some(found);
        }
    }

    // 3. Windows also supports a per-user install under %LOCALAPPDATA%.
    #[cfg(windows)]
    if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
        let path = PathBuf::from(local_appdata).join(r"Google\Chrome\Application\chrome.exe");
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
    }

    None
}

async fn try_chrome_native() -> Option<(ManagedBrowser, String)> {
    let bin = find_chrome()?;
    tracing::info!("Auto-detected Chrome: {bin}");

    // Own the profile dir so Chrome does not create an un-cleaned one in the
    // OS temp dir. If it cannot be created, or a sandboxed Chrome (snap,
    // flatpak) cannot use it, fall back to Chrome's default: rendering wins.
    if let Some(profile) = create_profile_dir(&profile_root()) {
        if let Some(found) = launch_chrome(&bin, Some(profile)).await {
            return Some(found);
        }
        tracing::warn!("Chrome did not start with an owned profile dir, retrying without it");
    }
    launch_chrome(&bin, None).await
}

async fn launch_chrome(bin: &str, profile: Option<PathBuf>) -> Option<(ManagedBrowser, String)> {
    let mut cmd = Command::new(bin);
    if let Some(dir) = &profile {
        let mut flag = std::ffi::OsString::from("--user-data-dir=");
        flag.push(dir);
        cmd.arg(flag);
    }
    cmd.args([
        "--headless",
        "--disable-gpu",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--remote-debugging-port=0",
        "--remote-allow-origins=*",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    // Own process group so a group-kill reaps Chrome's zygote + renderer
    // children, not just the parent PID (rust-lang/rust#115241).
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("Failed to spawn Chrome: {e}");
            if let Some(dir) = &profile {
                lock_profiles().remove(dir);
                remove_profile_dir(dir);
            }
            return None;
        }
    };

    // Take stderr before moving `child` into the guard; register the pgid
    // BEFORE reading the WS URL so a Ctrl-C during startup still reaps it.
    let stderr = child.stderr.take()?;
    let pgid = register_child(&child);
    let guard = ManagedBrowser {
        kind: BrowserKind::Process {
            child,
            pgid,
            profile,
        },
    };

    let ws_url = read_ws_url_from_stderr(stderr).await?; // guard drops on None
    tracing::info!("Chrome CDP endpoint: {ws_url}");
    Some((guard, ws_url))
}

// --- Shared helpers ---

/// Find an available TCP port by binding to port 0.
fn find_available_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Poll a CDP endpoint's /json/version until it responds with a webSocketDebuggerUrl.
async fn poll_cdp_endpoint(port: u16, timeout_secs: u64) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    while tokio::time::Instant::now() < deadline {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(json) = resp.json::<serde_json::Value>().await
            && let Some(ws_url) = json.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
        {
            return Some(ws_url.to_string());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    None
}

/// Read the WebSocket URL from a browser's stderr pipe.
/// Chrome prints "DevTools listening on ws://...", LightPanda prints "Listening on ws://...".
async fn read_ws_url_from_stderr(stderr: tokio::process::ChildStderr) -> Option<String> {
    let mut reader = BufReader::new(stderr).lines();

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(Some(line)) = reader.next_line().await {
            // Chrome: "DevTools listening on ws://127.0.0.1:PORT/devtools/browser/UUID"
            if let Some(url) = line.strip_prefix("DevTools listening on ") {
                return Some(url.trim().to_string());
            }
            // LightPanda or other: "Listening on ws://..."
            if let Some(start) = line.find("ws://") {
                return Some(line[start..].trim().to_string());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Resolve `name` against the `PATH` directories, returning its absolute path.
///
/// Implemented against `PATH` directly rather than shelling out to `which`,
/// which does not exist on Windows (it is `where.exe` there) — that made every
/// PATH-based lookup silently fail on Windows.
fn find_in_path(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    find_in_dirs(name, std::env::split_paths(&path_var))
}

fn find_in_dirs(name: &str, dirs: impl Iterator<Item = PathBuf>) -> Option<String> {
    // On Windows a bare name is resolved by appending an executable extension.
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };

    for dir in dirs {
        for ext in extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_executable_file(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn is_executable_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    // Windows has no executable bit — the extension is the signal, and
    // `find_in_dirs` already constrains that.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    true
}

fn command_exists(name: &str) -> bool {
    find_in_path(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile registry is process-global; tests touching it take turns.
    static REGISTRY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn scratch_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("crw-profile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        root
    }

    #[test]
    fn profile_dir_is_created_registered_and_removed_on_teardown() {
        let _turn = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch_root("create");
        let dir = create_profile_dir(&root).expect("profile dir");
        assert!(dir.starts_with(&root) && dir.is_dir());
        assert!(lock_profiles().contains(&dir));

        remove_registered_profiles();
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_removes_dead_owner_but_keeps_live_and_own() {
        let _turn = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch_root("sweep");
        // No real process has this pid (above every OS pid limit, below i32::MAX).
        let dead = root.join("2000000000-1");
        let live = create_profile_dir(&root).expect("registered profile");
        let prev_run = root.join(format!("{}-1", std::process::id()));
        let unrelated = root.join("not-a-profile");
        for d in [&dead, &prev_run, &unrelated] {
            std::fs::create_dir_all(d).unwrap();
        }

        sweep_stale_profiles(&root);

        assert!(!dead.exists(), "dead owner's profile must be swept");
        assert!(live.exists(), "a registered profile must survive");
        assert!(
            !prev_run.exists(),
            "a same-pid dir from a previous run is stale"
        );
        assert!(unrelated.exists(), "unparseable names must survive");
        remove_registered_profiles();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pid_alive_distinguishes_self_from_nonexistent() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(2_000_000_000));
    }

    /// Create a throwaway directory holding a single fake executable.
    fn dir_with_executable(tag: &str, file_name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("crw-find-in-dirs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let bin = dir.join(file_name);
        std::fs::write(&bin, b"#!/bin/sh\n").expect("write fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake binary");
        }
        dir
    }

    #[test]
    fn finds_executable_by_bare_name() {
        // Windows resolves a bare name through an executable extension; Unix
        // does not. Either way `find_in_dirs("fake-browser")` must resolve.
        let file_name = if cfg!(windows) {
            "fake-browser.exe"
        } else {
            "fake-browser"
        };
        let dir = dir_with_executable("hit", file_name);

        let found = find_in_dirs("fake-browser", std::iter::once(dir.clone()))
            .expect("executable on PATH must be found");
        assert_eq!(PathBuf::from(found), dir.join(file_name));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_non_executable_and_missing_names() {
        let dir = dir_with_executable("miss", "fake-browser");
        assert!(find_in_dirs("other-browser", std::iter::once(dir.clone())).is_none());

        // A plain, non-executable file must not be mistaken for a binary.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let data = dir.join("not-a-binary");
            std::fs::write(&data, b"data").expect("write data file");
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644))
                .expect("chmod data file");
            assert!(find_in_dirs("not-a-binary", std::iter::once(dir.clone())).is_none());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
