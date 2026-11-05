const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const install = path.join(__dirname, "..", "bin", "install.js");

function runInstall(home, extraArgs = []) {
  return spawnSync(process.execPath, [install, "--codex", ...extraArgs], {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      CRW_API_KEY: "",
    },
  });
}

test("install registers a launchable local MCP server in a clean Codex home", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-install-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));

  const first = runInstall(home);
  assert.equal(first.status, 0, first.stderr || first.stdout);

  const skill = fs.readFileSync(
    path.join(home, ".codex", "skills", "crw", "SKILL.md"),
    "utf8",
  );
  assert.match(skill, /^---\nname: crw\n/m);

  const configPath = path.join(home, ".codex", "config.toml");
  const config = fs.readFileSync(configPath, "utf8");
  assert.match(config, /\[mcp_servers\.crw\]/);
  assert.match(config, /command = "npx"/);
  assert.match(config, /args = \["-y", "crw-mcp"\]/);
  assert.doesNotMatch(config, /CRW_API_KEY/);

  const second = runInstall(home);
  assert.equal(second.status, 0, second.stderr || second.stdout);
  const configAfterSecondInstall = fs.readFileSync(configPath, "utf8");
  assert.equal(
    (configAfterSecondInstall.match(/\[mcp_servers\.crw\]/g) || []).length,
    1,
    "re-running install must not duplicate the MCP registration",
  );
});

test("re-running from config removes stale Codex credential sections", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-codex-replace-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const configPath = path.join(home, ".codex", "config.toml");
  fs.mkdirSync(path.dirname(configPath), { recursive: true });
  fs.writeFileSync(
    configPath,
    '[model]\nname = "keep-me"\n\n[mcp_servers.crw]\ncommand = "old"\n\n[mcp_servers.crw.env]\nCRW_API_KEY = "fc-stale"\n\n[mcp_servers.other]\ncommand = "keep"\n',
    "utf8",
  );

  const result = runInstall(home, ["--from-config"]);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const config = fs.readFileSync(configPath, "utf8");
  assert.match(config, /name = "keep-me"/);
  assert.match(config, /\[mcp_servers\.other\]/);
  assert.doesNotMatch(config, /fc-stale|CRW_API_KEY|command = "old"/);
  assert.equal((config.match(/\[mcp_servers\.crw\]/g) || []).length, 1);
});

test("--from-config does not copy a shell API key into agent config", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-from-config-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));

  const result = spawnSync(process.execPath, [install, "--codex", "--from-config"], {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      CRW_API_KEY: "fc-must-not-be-copied",
      CRW_API_URL: "https://api.fastcrw.com",
    },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const config = fs.readFileSync(path.join(home, ".codex", "config.toml"), "utf8");
  assert.doesNotMatch(config, /fc-must-not-be-copied|CRW_API_KEY|CRW_API_URL/);
  assert.match(result.stdout, /follows ~\/\.config\/crw\/config\.toml/);
});

test("malformed existing JSON is preserved instead of overwritten", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-malformed-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const configPath = path.join(home, ".cursor", "mcp.json");
  fs.mkdirSync(path.dirname(configPath), { recursive: true });
  fs.writeFileSync(configPath, "{ definitely not json\n", "utf8");

  const result = spawnSync(process.execPath, [install, "--cursor", "--from-config"], {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      CRW_API_KEY: "",
    },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /refusing to overwrite malformed JSON/);
  assert.equal(fs.readFileSync(configPath, "utf8"), "{ definitely not json\n");
});

test("existing agent config is backed up before the first merge", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-backup-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const configPath = path.join(home, ".cursor", "mcp.json");
  const original = '{"theme":"dark"}\n';
  fs.mkdirSync(path.dirname(configPath), { recursive: true });
  fs.writeFileSync(configPath, original, "utf8");

  const result = spawnSync(process.execPath, [install, "--cursor", "--from-config"], {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      CRW_API_KEY: "",
    },
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.equal(fs.readFileSync(`${configPath}.crw-backup`, "utf8"), original);
  assert.equal(JSON.parse(fs.readFileSync(configPath, "utf8")).theme, "dark");
});

test("install rejects an unknown flag and writes nothing", (t) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "crw-mcp-unknown-flag-"));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));

  const result = runInstall(home, ["--dry-run"]);
  assert.equal(result.status, 2, result.stdout);
  assert.match(result.stderr, /unknown argument "--dry-run"/);
  assert.equal(fs.existsSync(path.join(home, ".codex")), false);

  // A flag's value is not mistaken for an unknown argument.
  const ok = runInstall(home, ["--api-url", "https://api.example"]);
  assert.equal(ok.status, 0, ok.stderr || ok.stdout);
});
