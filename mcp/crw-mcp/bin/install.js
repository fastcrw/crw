#!/usr/bin/env node

// `crw-mcp install [--<agent>] [--api-key <key>] [--api-url <url>]`
// Installs BOTH the crw skill AND the crw MCP server into the target agents.
// (`crw-mcp init` installs the skill only.) Without an agent flag it targets
// every detected agent; with no API key it wires local mode (free, embedded).

const {
  AGENTS,
  detectAgents,
  getApiKey,
  readSkill,
  installSkill,
  installMcp,
  home,
} = require("./agents.js");

const args = process.argv.slice(2);

function argValue(flag) {
  const i = args.indexOf(flag);
  return i !== -1 && args[i + 1] ? args[i + 1] : null;
}

if (args.includes("--help") || args.includes("-h")) {
  console.log(`
crw-mcp install — Install the CRW skill AND MCP server into your AI agents

Usage:
  npx crw-mcp@latest install [options]

Options:
  --all            Install to all detected agents
  --claude-code    Claude Code      --codex      Codex
  --cursor         Cursor           --opencode   OpenCode
  --gemini-cli     Gemini CLI       --windsurf   Windsurf
  --api-key <key>  fastcrw.com API key → cloud mode (else local/embedded)
  --api-url <url>  API base (default https://api.fastcrw.com when a key is set)
  --from-config    Read mode from ~/.config/crw/config.toml at runtime
  -h, --help       Show this help

Without flags, auto-detects installed agents. Installs the skill (SKILL.md) and
registers the "crw" MCP server. Run \`crw-mcp init\` for the skill only.
`);
  process.exit(0);
}

// Anything unrecognised stops here: ignoring it would turn a typo, or a flag
// this command does not have (such as --dry-run), into a real install.
const VALUE_FLAGS = ["--api-key", "--api-url"];
const KNOWN_FLAGS = ["--all", "--from-config", ...AGENTS.map((a) => a.flag)];
for (let i = 0; i < args.length; i++) {
  if (VALUE_FLAGS.includes(args[i])) {
    i++;
  } else if (!KNOWN_FLAGS.includes(args[i])) {
    console.error(`crw-mcp install: unknown argument "${args[i]}". Nothing was installed.`);
    console.error("Run `npx crw-mcp@latest install --help` to see the options.");
    process.exit(2);
  }
}

const fromConfig = args.includes("--from-config");
const apiKey = fromConfig ? null : getApiKey(args);
const apiUrl = argValue("--api-url") || (apiKey ? "https://api.fastcrw.com" : null);
const env = apiKey ? { CRW_API_KEY: apiKey, CRW_API_URL: apiUrl } : {};

const agents = detectAgents(args);
if (agents.length === 0) {
  console.log("No supported AI agents detected.");
  console.log("Supported: " + AGENTS.map((a) => a.name).join(", "));
  console.log("\nPass an agent flag (e.g. --claude-code) to install anyway.");
  process.exit(1);
}

const skill = readSkill();
const results = [];
const failures = [];
for (const agent of agents) {
  try {
    const skillPath = installSkill(agent, skill);
    const mcpLabel = installMcp(agent, env);
    results.push({ name: agent.name, skillPath, mcpLabel });
  } catch (err) {
    console.error(`  ${agent.name}: ${err.message}`);
    failures.push(agent.name);
  }
}

if (results.length === 0) {
  console.log("Failed to install to any agent.");
  process.exit(1);
}

console.log("crw installed — skill + MCP:\n");
for (const r of results) {
  console.log(`  ${r.name}`);
  console.log(`    skill: ${r.skillPath.replace(home, "~")}`);
  console.log(`    ${r.mcpLabel.replace(home, "~")}`);
}

if (fromConfig) {
  console.log("\nMode: follows ~/.config/crw/config.toml (credentials are not copied into agent configs).");
} else if (apiKey) {
  console.log(`\nMode: cloud — CRW_API_KEY ${apiKey.slice(0, 10)}… → ${apiUrl}`);
} else {
  console.log("\nMode: local (free, embedded binary — no key needed).");
  console.log("  Cloud mode: re-run with --api-key crw_live_… (1000 free credits at https://fastcrw.com).");
}
console.log("\nRestart your agent to load the MCP server.");
console.log("Docs: https://fastcrw.com/docs");
if (failures.length > 0) {
  console.error(`\nFailed targets: ${failures.join(", ")}. Existing configs were preserved.`);
  process.exitCode = 1;
}
