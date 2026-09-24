"""Download, cache, and locate the crw-mcp binary."""

from __future__ import annotations

import hashlib
import importlib.metadata
import os
import stat
import tarfile
import zipfile
from io import BytesIO
from pathlib import Path
from urllib.request import Request, urlopen

from platformdirs import user_cache_dir

from crw._platform import BINARY_NAME, get_asset_name
from crw.exceptions import CrwBinaryNotFoundError

GITHUB_REPO = "fastcrw/crw"
_RELEASES = f"https://github.com/{GITHUB_REPO}/releases/download"

_HINT = (
    "Set CRW_BINARY=/path/to/crw-mcp to use a binary you already have, "
    "or install one with: cargo install crw-mcp"
)


def _release_tag() -> str:
    """Git tag of the release this SDK was published alongside.

    Resolved from our own installed version rather than the mutable
    `releases/latest`, so a wrapper only ever asks for the tag it shipped with.
    """
    try:
        version = importlib.metadata.version("crw")
    except importlib.metadata.PackageNotFoundError as e:
        raise CrwBinaryNotFoundError(
            f"crw is importable but not installed as a distribution, so its "
            f"release tag cannot be determined. {_HINT}"
        ) from e

    # Every tag this project has published is a plain vX.Y.Z. If a prerelease
    # ever ships, PEP 440 normalisation (v0.28.0-alpha1 -> 0.28.0a1) will make
    # this 404 loudly on first use, which beats reconstructing a tag by guess.
    return "v" + version


def _cache_dir(tag: str) -> Path:
    return Path(user_cache_dir("crw")) / tag


def _expected_digest(tag: str, asset: str) -> str:
    """SHA-256 recorded for `asset` in the release's SHA256SUMS.

    Fails closed: a missing or unlisted checksum aborts rather than falling
    through to an unverified download.
    """
    url = f"{_RELEASES}/{tag}/SHA256SUMS"
    req = Request(url, headers={"User-Agent": f"crw-python/{tag}"})
    try:
        with urlopen(req, timeout=30) as resp:
            text = resp.read().decode("utf-8", "replace")
    except OSError as e:
        # HTTPError is an OSError. Only 404 maps to a different user action:
        # this release predates SHA256SUMS, so pin a newer one.
        if getattr(e, "code", None) == 404:
            raise CrwBinaryNotFoundError(
                f"release {tag} publishes no SHA256SUMS, so {asset} cannot be "
                f"verified. {_HINT}"
            ) from e
        raise CrwBinaryNotFoundError(
            f"could not reach GitHub to fetch SHA256SUMS for {tag}: {e}. {_HINT}"
        ) from e

    for line in text.splitlines():
        parts = line.split()
        # coreutils writes "<hash>  <name>" or "<hash> *<name>" in binary mode.
        if len(parts) == 2 and parts[1].lstrip("*") == asset:
            return parts[0].lower()

    raise CrwBinaryNotFoundError(
        f"{asset} is not listed in SHA256SUMS for {tag}. {_HINT}"
    )


def _download_binary(tag: str) -> Path:
    """Download the release asset, verify it, then unpack it into the cache."""
    asset = get_asset_name()
    if asset is None:
        raise CrwBinaryNotFoundError(
            "No prebuilt binary for this platform. "
            "Install from source: cargo install crw-mcp"
        )

    expected = _expected_digest(tag, asset)

    url = f"{_RELEASES}/{tag}/{asset}"
    req = Request(url, headers={"User-Agent": f"crw-python/{tag}"})
    try:
        with urlopen(req, timeout=120) as resp:
            data = resp.read()
    except OSError as e:
        raise CrwBinaryNotFoundError(
            f"could not download {asset} from {tag}: {e}. {_HINT}"
        ) from e

    # Verified before anything touches disk: the archive is already fully in
    # memory, so an unverified byte is never written at all.
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        raise CrwBinaryNotFoundError(
            f"checksum mismatch for {asset} from {tag}: expected {expected}, "
            f"got {actual}. Refusing to run it."
        )

    cache = _cache_dir(tag)
    cache.mkdir(parents=True, exist_ok=True)

    if asset.endswith(".tar.gz"):
        with tarfile.open(fileobj=BytesIO(data), mode="r:gz") as tar:
            for member in tar.getmembers():
                # isreg() refuses links and devices, and we choose the
                # destination path, so traversal is impossible by construction.
                # This needs no `filter=`, which only exists on 3.10.12+ while
                # we support >=3.10.
                if member.isreg() and member.name.endswith("crw-mcp"):
                    (cache / BINARY_NAME).write_bytes(tar.extractfile(member).read())
                    break
            else:
                raise CrwBinaryNotFoundError(f"crw-mcp not found in {asset}")
    elif asset.endswith(".zip"):
        with zipfile.ZipFile(BytesIO(data)) as zf:
            for name in zf.namelist():
                if name.endswith("crw-mcp.exe") or name.endswith("crw-mcp"):
                    target = cache / BINARY_NAME
                    target.write_bytes(zf.read(name))
                    break
            else:
                raise CrwBinaryNotFoundError(f"crw-mcp not found in {asset}")

    binary = cache / BINARY_NAME
    binary.chmod(binary.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return binary


def _is_native_binary(path: Path) -> bool:
    """Check if a file is a native binary (not a Python/shell script)."""
    try:
        with open(path, "rb") as f:
            header = f.read(4)
        # ELF, Mach-O, or PE magic bytes
        return header[:4] in (
            b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe",
            b"MZ\x90\x00", b"MZ\x00\x00",
        )
    except OSError:
        return False


def ensure_binary() -> Path:
    """Return path to the crw-mcp binary, downloading it if necessary."""
    # 1. Check CRW_BINARY env override
    env_path = os.environ.get("CRW_BINARY")
    if env_path:
        p = Path(env_path)
        if p.is_file():
            return p
        raise CrwBinaryNotFoundError(f"CRW_BINARY={env_path} does not exist")

    # 2. Check if crw-mcp native binary is on PATH (e.g. cargo install)
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        # An empty PATH entry means "current directory"; a trailing colon is
        # common enough that this would otherwise exec ./crw-mcp from any cwd.
        if not directory:
            continue
        candidate = Path(directory) / BINARY_NAME
        if candidate.is_file() and _is_native_binary(candidate):
            return candidate

    # 3. Already downloaded and verified for this exact version. No network
    #    call happens on a cache hit, which is also the offline path.
    #    ponytail: this trusts the cache. Re-hashing against a sidecar digest
    #    would not help, since anything able to rewrite the binary can rewrite
    #    the sidecar too; closing it properly needs a digest baked into the
    #    wrapper at build time. Documented in SECURITY.md rather than faked.
    tag = _release_tag()
    binary = _cache_dir(tag) / BINARY_NAME
    if binary.is_file() and os.access(binary, os.X_OK):
        return binary

    # 4. Download and verify.
    return _download_binary(tag)
