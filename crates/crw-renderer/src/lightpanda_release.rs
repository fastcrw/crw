//! The one LightPanda release every crw download path installs.
//!
//! Pinned to a tag, never `nightly`: upstream rebuilds the nightly in place and
//! it has regressed (lightpanda-io/browser#4477). Upstream publishes no
//! `.sha256` assets, so the checksums live here, copied from the GitHub release
//! asset digests of [`VERSION`]. Bump the version and every hash together.

use sha2::{Digest, Sha256};

pub const VERSION: &str = "1.0.4";

/// Release asset name and its SHA256 for the running platform, `None` where
/// crw installs no binary (Intel macOS is published upstream but not wired:
/// those hosts go straight to Chrome, as before).
pub fn asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some((
            "lightpanda-x86_64-linux",
            "7d22651129901203abdb95c64f49069c49e88f5211f4d8c4c6828c123c4adab4",
        )),
        ("linux", "aarch64") => Some((
            "lightpanda-aarch64-linux",
            "f021d49a0b66d07bf2633f8e00870b9075293516f872695b0ca4aeb5848b6ea8",
        )),
        ("macos", "aarch64") => Some((
            "lightpanda-aarch64-macos",
            "83b0ba5ae31086b675fb3ba50644bda075a13bebabe46dc5f7543952bc41ae46",
        )),
        _ => None,
    }
}

pub fn url(asset: &str) -> String {
    format!("https://github.com/lightpanda-io/browser/releases/download/{VERSION}/{asset}")
}

/// File name of the managed install under `~/.crw`. Versioned so an existing
/// install of another build is never reused as if it were this one.
pub fn managed_file_name() -> String {
    format!("lightpanda-{VERSION}")
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_a_known_digest() {
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn url_points_at_the_pinned_tag() {
        assert_eq!(
            url("lightpanda-x86_64-linux"),
            "https://github.com/lightpanda-io/browser/releases/download/1.0.4/lightpanda-x86_64-linux"
        );
    }
}
