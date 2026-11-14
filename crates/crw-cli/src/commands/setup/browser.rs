//! Browser engine setup (LightPanda, Chrome).

use crate::commands::setup::shell::home_dir;
use crate::commands::setup::ui;
use crw_renderer::lightpanda_release;
use indicatif::{ProgressBar, ProgressStyle};
use std::env::consts::{ARCH, OS};
use std::path::PathBuf;

/// Find an executable in PATH using platform-specific command.
/// Returns the path if found and validated, None otherwise.
fn find_in_path(name: &str) -> Option<PathBuf> {
    // Use platform-specific command
    #[cfg(windows)]
    let cmd = "where";
    #[cfg(not(windows))]
    let cmd = "which";

    let output = std::process::Command::new(cmd).arg(name).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let path_str = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()? // Take first line (Windows `where` can return multiple)
        .trim()
        .to_string();

    if path_str.is_empty() {
        return None;
    }

    let path = PathBuf::from(&path_str);

    // Validate the path exists and is a file
    if path.exists() && path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// Browser engine choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEngine {
    LightPanda,
    Chrome,
    None,
}

impl BrowserEngine {
    /// Display name for the browser engine.
    #[allow(dead_code)]
    pub fn name(&self) -> &'static str {
        match self {
            BrowserEngine::LightPanda => "LightPanda",
            BrowserEngine::Chrome => "Chrome/Chromium",
            BrowserEngine::None => "None (HTTP only)",
        }
    }
}

/// Platform support for LightPanda.
pub struct PlatformInfo {
    pub os_label: &'static str,
    pub arch_label: &'static str,
}

/// Get platform info for LightPanda download.
pub fn get_platform_info() -> Option<PlatformInfo> {
    match (OS, ARCH) {
        ("linux", "x86_64") => Some(PlatformInfo {
            os_label: "Linux",
            arch_label: "x86_64",
        }),
        ("linux", "aarch64") => Some(PlatformInfo {
            os_label: "Linux",
            arch_label: "aarch64",
        }),
        ("macos", "aarch64") => Some(PlatformInfo {
            os_label: "macOS",
            arch_label: "aarch64 (Apple Silicon)",
        }),
        _ => None,
    }
}

/// Check if Chrome/Chromium is installed.
pub fn detect_chrome() -> Option<PathBuf> {
    let candidates = match OS {
        "macos" => vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ],
        "linux" => vec![
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ],
        "windows" => vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ],
        _ => vec![],
    };

    for path in candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // Also check PATH using platform-specific lookup
    if let Some(path) = find_in_path("google-chrome") {
        return Some(path);
    }
    if let Some(path) = find_in_path("chromium") {
        return Some(path);
    }
    if let Some(path) = find_in_path("chrome") {
        return Some(path);
    }

    None
}

/// Check if LightPanda is already installed.
pub fn detect_lightpanda() -> Option<PathBuf> {
    // Keep setup and the runtime on the same managed location. The runtime
    // checks ~/.crw/lightpanda-<version> directly, so no PATH or shell-rc mutation is
    // required after setup.
    if let Some(managed_path) = lightpanda_managed_path()
        && managed_path.exists()
        && managed_path.is_file()
    {
        return Some(managed_path);
    }

    // Check PATH using platform-specific lookup
    find_in_path("lightpanda")
}

/// Download LightPanda with progress bar.
pub async fn download_lightpanda() -> Result<PathBuf, String> {
    let unsupported = || {
        format!(
            "Unsupported platform: {} {}. LightPanda provides binaries for Linux x86_64, Linux aarch64 and macOS aarch64.",
            OS, ARCH
        )
    };
    let platform = get_platform_info().ok_or_else(unsupported)?;
    let (asset, expected_hash) = lightpanda_release::asset().ok_or_else(unsupported)?;

    ui::print_info(&format!(
        "Detected: {} {}",
        platform.os_label, platform.arch_label
    ));

    let install_path = lightpanda_managed_path()
        .ok_or_else(|| "Could not determine home directory for LightPanda install".to_string())?;
    let install_dir = install_path
        .parent()
        .expect("managed LightPanda path always has a parent");

    // Create install directory
    std::fs::create_dir_all(install_dir)
        .map_err(|e| format!("Failed to create {}: {}", install_dir.display(), e))?;

    // Download the pinned release and check it against the hash shipped in crw.
    let url = lightpanda_release::url(asset);
    let bytes = download_with_progress(&url, "LightPanda").await?;
    let actual_hash = lightpanda_release::sha256_hex(&bytes);
    if actual_hash != expected_hash {
        return Err(format!(
            "SHA256 checksum mismatch, LightPanda {} was not installed.\nExpected: {}\nActual: {}\nThe download may be corrupted or tampered with. Run setup again.",
            lightpanda_release::VERSION,
            expected_hash,
            actual_hash
        ));
    }
    ui::print_success(&format!("SHA256 verified: {}...", &actual_hash[..12]));

    // Write beside the target and rename, so an interrupted setup never leaves
    // a partial file at the path the runtime trusts without re-hashing.
    let mut part_path = install_path.clone().into_os_string();
    part_path.push(format!(".part-{}", std::process::id()));
    let part_path = PathBuf::from(part_path);
    let install = || -> Result<(), String> {
        std::fs::write(&part_path, &bytes)
            .map_err(|e| format!("Failed to write {}: {}", part_path.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&part_path, perms)
                .map_err(|e| format!("Failed to chmod +x: {}", e))?;
        }
        std::fs::rename(&part_path, &install_path)
            .map_err(|e| format!("Failed to install {}: {}", install_path.display(), e))
    };
    if let Err(e) = install() {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }

    ui::print_success(&format!("Installed to {}", install_path.display()));

    Ok(install_path)
}

/// Same versioned file the runtime launcher looks for, so an install of any
/// other build is never mistaken for the pinned release.
fn lightpanda_managed_path() -> Option<PathBuf> {
    home_dir().map(|home| {
        home.join(".crw")
            .join(lightpanda_release::managed_file_name())
    })
}

/// Download a file with progress bar.
async fn download_with_progress(url: &str, name: &str) -> Result<Vec<u8>, String> {
    use futures::StreamExt;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch {}: {}", url, e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {}", resp.status(), url));
    }

    let total_size = resp.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("    [{bar:40.cyan/blue}] {percent}% ({bytes}/{total_bytes})")
            .unwrap()
            .progress_chars("█▓░"),
    );
    pb.set_message(format!("Downloading {}", name));

    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                bytes.extend_from_slice(&chunk);
                pb.set_position(bytes.len() as u64);
            }
            Err(e) => {
                pb.finish_and_clear();
                return Err(format!("Download error: {}", e));
            }
        }
    }

    pb.finish_and_clear();
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_platform_info() {
        // This test is platform-dependent
        let info = get_platform_info();
        // On supported platforms, should return Some
        // On unsupported platforms, returns None
        let _ = info;
    }
}
