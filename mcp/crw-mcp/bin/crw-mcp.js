#!/usr/bin/env node

const { spawnSync } = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");

const VERSION = require("../package.json").version;
const REPO = "fastcrw/crw";

// Each platform has a prebuilt npm package (fast path, installed via
// optionalDependencies) AND a matching GitHub release asset (fallback path,
// downloaded on first run). The fallback makes the launcher robust to a
// missing/unpublishable platform package (e.g. npm security-held names) — no
// single package can freeze the channel, mirroring the PyPI launcher.
const PLATFORMS = {
  "darwin-x64": { pkg: "crw-mcp-darwin-x64", asset: "crw-mcp-darwin-x64.tar.gz" },
  "darwin-arm64": { pkg: "crw-mcp-darwin-arm64", asset: "crw-mcp-darwin-arm64.tar.gz" },
  "linux-x64": { pkg: "crw-mcp-linux-x64", asset: "crw-mcp-linux-x64.tar.gz" },
  "linux-arm64": { pkg: "crw-mcp-linux-arm64", asset: "crw-mcp-linux-arm64.tar.gz" },
  "win32-x64": { pkg: "crw-mcp-win32-x64", asset: "crw-mcp-win32-x64.zip" },
  "win32-arm64": { pkg: "crw-mcp-win32-arm64", asset: "crw-mcp-win32-arm64.zip" },
};

const key = `${process.platform}-${process.arch}`;
const plat = PLATFORMS[key];

const binName = `crw-mcp${process.platform === "win32" ? ".exe" : ""}`;

// 1. Explicit override.
function fromEnv() {
  const p = process.env.CRW_MCP_BINARY || process.env.CRW_BINARY;
  return p && fs.existsSync(p) ? p : null;
}

// 2. Prebuilt platform package (fast path — no download).
function fromPackage() {
  try {
    const bin = path.join(
      path.dirname(require.resolve(`${plat.pkg}/package.json`)),
      binName
    );
    return fs.existsSync(bin) ? bin : null;
  } catch {
    return null;
  }
}

function cacheDir() {
  const base =
    process.platform === "win32"
      ? process.env.LOCALAPPDATA || path.join(os.homedir(), "AppData", "Local")
      : process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
  return path.join(base, "crw-mcp", `v${VERSION}`);
}

// Always resolves a Buffer: the archive is verified in memory, so an unverified
// byte never reaches disk at all.
function httpsGet(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": `crw-mcp/${VERSION}` } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          if (redirects > 5) return reject(new Error("too many redirects"));
          return resolve(httpsGet(res.headers.location, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

// Parse one entry out of a coreutils-style SHA256SUMS. Written on Linux, read
// here on every platform, so tolerate CRLF and the "*" binary-mode marker.
function digestFor(sumsText, asset) {
  for (const line of sumsText.split(/\r?\n/)) {
    const parts = line.trim().split(/\s+/);
    if (parts.length === 2 && parts[1].replace(/^\*/, "") === asset)
      return parts[0].toLowerCase();
  }
  return null;
}

// 3. Download the matching release asset and extract it (cached per version).
async function fromDownload() {
  const dir = cacheDir();
  const bin = path.join(dir, binName);
  if (fs.existsSync(bin)) return bin;

  fs.mkdirSync(dir, { recursive: true });
  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  const url = `${base}/${plat.asset}`;

  // Fail closed: fetch the expected digest first, so a release without one is
  // refused before anything is downloaded rather than after.
  let sums;
  try {
    sums = (await httpsGet(`${base}/SHA256SUMS`)).toString("utf8");
  } catch (e) {
    // Only a 404 means the release genuinely lacks checksums. Reporting a
    // proxy or DNS failure as "our release is broken" sends users to file
    // issues against a release that is fine.
    throw new Error(
      /^HTTP 404 /.test(e.message)
        ? `release v${VERSION} publishes no SHA256SUMS, so ${plat.asset} cannot be verified`
        : `could not fetch SHA256SUMS for v${VERSION}: ${e.message}`
    );
  }
  const expected = digestFor(sums, plat.asset);
  if (!expected) {
    throw new Error(`${plat.asset} is not listed in SHA256SUMS for v${VERSION}`);
  }

  // stderr only: stdout is the MCP (JSON-RPC) channel.
  console.error(`crw-mcp: downloading ${plat.asset} (v${VERSION})...`);
  const data = await httpsGet(url);

  const actual = crypto.createHash("sha256").update(data).digest("hex");
  if (actual !== expected) {
    throw new Error(
      `checksum mismatch for ${plat.asset}: expected ${expected}, got ${actual}. ` +
        `Refusing to run it.`
    );
  }
  // Stage per process, so two cold starts (MCP clients do spawn several at
  // once on first configure) cannot truncate each other's archive or delete
  // each other's extraction. The binary is moved in only once it is whole:
  // `tar` applies the archive's mode from the first byte and the cache hit
  // above gates on bare existsSync, so an interrupted extraction (Ctrl-C, disk
  // full, OOM) would otherwise leave a truncated executable that every later
  // run spawns. Python needs no staging: it writes the member itself, so the
  // exec bit only arrives after the copy and its os.access(X_OK) gate rejects
  // a partial file and re-downloads.
  const stage = fs.mkdtempSync(path.join(dir, ".stage-"));
  const archive = path.join(stage, plat.asset);
  try {
    fs.writeFileSync(archive, data);
    const tarArgs = plat.asset.endsWith(".zip")
      ? ["-xf", archive, "-C", stage]
      : ["-xzf", archive, "-C", stage];
    const x = spawnSync("tar", tarArgs, { stdio: "inherit" });
    if (x.status !== 0) {
      throw new Error(`failed to extract ${plat.asset} (tar exit ${x.status})`);
    }
    const staged = path.join(stage, binName);
    if (!fs.existsSync(staged)) {
      throw new Error(`binary ${binName} not found in ${plat.asset}`);
    }
    if (process.platform !== "win32") fs.chmodSync(staged, 0o755);
    fs.renameSync(staged, bin);
  } finally {
    fs.rmSync(stage, { recursive: true, force: true });
  }
  return bin;
}

async function resolveBinary() {
  return fromEnv() || fromPackage() || (await fromDownload());
}

module.exports = { digestFor, fromDownload, cacheDir, plat, binName };

// Only run when invoked as the CLI, so tests can require this file.
if (require.main === module) {
  // Handle install/init subcommands before delegating to the Rust binary.
  // `init` = skill only; `install` = skill + MCP server. Both are file copies
  // that need no binary, so they must run before the platform check: a 32-bit
  // Pi or FreeBSD user can still install the skill.
  if (process.argv[2] === "init") {
    require("./init.js");
    process.exit(0);
  }
  if (process.argv[2] === "install") {
    require("./install.js");
    process.exit(0);
  }
  if (!plat) {
    console.error(
      `crw-mcp: unsupported platform ${key}. Supported: ${Object.keys(PLATFORMS).join(", ")}`
    );
    process.exit(1);
  }
  resolveBinary()
  .then((bin) => {
    const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
    process.exit(result.status ?? 1);
  })
  .catch((err) => {
    console.error(
      `crw-mcp: could not locate or download the ${key} binary.\n  ${err.message}\n` +
        `  Set CRW_MCP_BINARY=/path/to/crw-mcp to use a local build, or install\n` +
        `  from https://github.com/${REPO}/releases.`
    );
    process.exit(1);
  });
}
