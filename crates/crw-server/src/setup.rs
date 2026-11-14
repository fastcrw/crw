//! Interactive setup command that downloads LightPanda and creates a local config.

use crw_renderer::lightpanda_release;
use std::env::consts::{ARCH, OS};
use std::path::PathBuf;

/// Run the interactive setup: download LightPanda binary and create config.
pub async fn run_setup() {
    println!();
    let Some((binary_name, expected_hash)) = lightpanda_release::asset() else {
        eprintln!("  ✗ Unsupported platform: {OS} {ARCH}");
        eprintln!(
            "    LightPanda provides binaries for Linux x86_64, Linux aarch64 and macOS aarch64."
        );
        std::process::exit(1);
    };

    println!("  → Detected: {OS} {ARCH}");

    // Download LightPanda binary.
    let install_dir = home_local_bin();
    let install_path = install_dir.join("lightpanda");

    println!(
        "  → Downloading LightPanda {}...",
        lightpanda_release::VERSION
    );

    let url = lightpanda_release::url(binary_name);
    let bytes = match download_binary(&url).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("  ✗ Download failed: {e}");
            std::process::exit(1);
        }
    };

    // Verify against the hash shipped in crw for this pinned release.
    let actual_hash = lightpanda_release::sha256_hex(&bytes);
    if actual_hash != expected_hash {
        eprintln!("  ✗ SHA256 checksum mismatch, LightPanda was not installed.");
        eprintln!("    Expected: {expected_hash}");
        eprintln!("    Actual:   {actual_hash}");
        eprintln!("    The download may be corrupted or tampered with. Run setup again.");
        std::process::exit(1);
    }
    println!("  ✓ SHA256 checksum verified: {actual_hash}");

    if let Err(e) = std::fs::create_dir_all(&install_dir) {
        eprintln!("  ✗ Failed to create {}: {e}", install_dir.display());
        std::process::exit(1);
    }

    // Write beside the target and rename: the runtime prefers a `lightpanda`
    // on PATH and never re-checks it, so a half-written file must never land there.
    let part_path = install_dir.join(format!("lightpanda.part-{}", std::process::id()));
    if let Err(e) = std::fs::write(&part_path, &bytes) {
        eprintln!("  ✗ Failed to write {}: {e}", part_path.display());
        let _ = std::fs::remove_file(&part_path);
        std::process::exit(1);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&part_path, perms) {
            eprintln!("  ✗ Failed to chmod +x: {e}");
            let _ = std::fs::remove_file(&part_path);
            std::process::exit(1);
        }
    }

    if let Err(e) = std::fs::rename(&part_path, &install_path) {
        eprintln!("  ✗ Failed to install {}: {e}", install_path.display());
        let _ = std::fs::remove_file(&part_path);
        std::process::exit(1);
    }

    println!("  ✓ Installed to {}", install_path.display());

    // Write config.local.toml if it doesn't exist.
    let config_path = PathBuf::from("config.local.toml");
    if config_path.exists() {
        println!("  ✓ config.local.toml already exists (skipped)");
    } else {
        let config_content = r#"[renderer]
mode = "lightpanda"

[renderer.lightpanda]
ws_url = "ws://127.0.0.1:9222/"
"#;
        if let Err(e) = std::fs::write(&config_path, config_content) {
            eprintln!("  ✗ Failed to write config.local.toml: {e}");
            std::process::exit(1);
        }
        println!("  ✓ Created config.local.toml");
    }

    println!();
    println!("  Start the server with JS rendering:");
    println!("    lightpanda serve --host 127.0.0.1 --port 9222 --block-private-networks &");
    println!("    crw-server");
    println!();
}

async fn download_binary(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    let client = reqwest::Client::builder()
        .redirect(crw_core::url_safety::safe_redirect_policy())
        .build()?;

    let resp = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    Ok(bytes.to_vec())
}

fn home_local_bin() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local").join("bin")
}
