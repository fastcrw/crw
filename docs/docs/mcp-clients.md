<div class="page-intro">
  <div class="page-kicker">Integrations</div>
  <h1>MCP Client Setup</h1>
  <p class="page-subtitle">Add CRW to Claude Code, Codex, Cursor, Windsurf, Cline, Continue, osaurus, and similar MCP hosts. This page is the copy-paste setup reference for both local embedded mode and fastcrw.com cloud mode.</p>
  <div class="page-capabilities">
    <div class="page-capability"><strong>Best for:</strong> host-by-host setup</div>
    <div class="page-capability"><strong>Local mode:</strong> <code>npx crw-mcp</code></div>
    <div class="page-capability"><strong>Cloud mode:</strong> <code>CRW_API_URL + CRW_API_KEY</code></div>
  </div>
  <div class="page-actions">
    <a class="page-btn primary" href="#mcp">Open MCP Overview</a>
    <a class="page-btn secondary" href="https://fastcrw.com" target="_blank" rel="noopener">Get fastcrw.com API Key</a>
  </div>
</div>

## Start here

Use one of these three patterns:

| Pattern | Best when | Copy-paste shape |
|---|---|---|
| Embedded local | You want zero server setup and local scrape/crawl/map/parse tools | `command: npx`, `args: ["crw-mcp"]` |
| fastcrw.com cloud | You want hosted infrastructure and always-on `crw_search` | same as local plus `CRW_API_URL` and `CRW_API_KEY` |
| HTTP transport | Your host supports HTTP MCP and you already run `crw-server` | point the client at `http://localhost:3000/mcp` |
| Browser automation | You need a real, stateful browser (click, nav, tree) | `command: crw-browse` (separate MCP server — self-hosted) |

## What tools you get

Embedded local mode (`crw-mcp`) exposes up to 9 tools:

- `crw_scrape`
- `crw_crawl`
- `crw_check_crawl_status`
- `crw_map`
- `crw_extract` — structured extraction across URLs (async job)
- `crw_check_extract_status` — poll an extract job
- `crw_cancel_extract` — cancel an extract job and return canonical status
- `crw_parse_file` — parse a local PDF (base64) to markdown, no OCR (always present)
- `crw_search` — only advertised when a search backend is configured; hidden otherwise

fastcrw.com cloud mode exposes all 9 tools:

- `crw_scrape`
- `crw_crawl`
- `crw_check_crawl_status`
- `crw_map`
- `crw_extract`
- `crw_check_extract_status`
- `crw_cancel_extract`
- `crw_parse_file`
- `crw_search` — always available (managed search backend)

Cloud mode also adds `crw_find_data` and `crw_get_data` for structured data sources; see [Data Sources](#data-sources).

Browser automation mode (`crw-browse`, separate server — v0.4.0+) exposes:

- `goto` — navigate the browser to an URL
- `tree` — accessibility snapshot of the current page

:::tip
If you only remember one rule, remember this one: local embedded mode is the easiest setup (up to 9 tools, with `crw_search` appearing automatically when a search backend is configured), and fastcrw.com cloud mode is the easiest way to get all 9 tools including always-on web search.
:::

## Claude Code

### Local embedded

```bash
claude mcp add crw -- npx -y crw-mcp
```

:::note
500 credits to start, up to 500 more from onboarding tasks, no card.
<a href="https://fastcrw.com/register?ref=docs-mcp-clients-inline" target="_blank" rel="noopener">Get your API key</a>
:::

### fastcrw.com cloud

```bash
claude mcp add crw \
  -e CRW_API_URL=https://api.fastcrw.com \
  -e CRW_API_KEY=YOUR_API_KEY \
  -- npx -y crw-mcp
```

### Local HTTP transport

```bash
claude mcp add --transport http crw http://localhost:3000/mcp
```

Useful commands:

- `claude mcp list`
- `claude mcp remove crw`

## OpenAI Codex CLI

### Local embedded

```bash
codex mcp add crw -- npx crw-mcp
```

Or edit `~/.codex/config.toml`:

```toml
[mcp_servers.crw]
command = "npx"
args = ["crw-mcp"]
```

### fastcrw.com cloud

```toml
[mcp_servers.crw]
command = "npx"
args = ["crw-mcp"]

[mcp_servers.crw.env]
CRW_API_URL = "https://api.fastcrw.com"
CRW_API_KEY = "YOUR_API_KEY"
```

:::note
Codex uses the same stdio MCP server as Claude Code. The only difference is where you register it.
:::

## Claude Desktop

Edit the config file for your OS:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%/Claude/claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

### Local embedded

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

Fully quit and restart Claude Desktop after editing the file.

## Cursor

Create or edit `~/.cursor/mcp.json` for global setup, or `.cursor/mcp.json` inside one project.

### Local embedded

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

You can also add the same config from Cursor's MCP UI under Settings.

## Windsurf

Edit `~/.codeium/windsurf/mcp_config.json`.

### Local embedded

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

:::note
Windsurf has a total MCP tool limit. CRW stays lightweight: local embedded mode exposes up to 9 tools (`crw_search` is hidden unless a search backend is configured), and cloud mode always exposes all 9 tools.
:::

## Cline

Edit the Cline MCP settings file:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` |
| Windows | `%APPDATA%/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` |
| Linux | `~/.config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` |

### Local embedded

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "alwaysAllow": ["crw_scrape", "crw_map"],
      "disabled": false
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "alwaysAllow": ["crw_scrape", "crw_map", "crw_search"],
      "disabled": false,
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

## Continue

Edit `~/.continue/config.yaml`.

### Local embedded

```yaml
mcpServers:
  - name: crw
    command: npx
    args:
      - crw-mcp
```

### fastcrw.com cloud

```yaml
mcpServers:
  - name: crw
    command: npx
    args:
      - crw-mcp
    env:
      CRW_API_URL: https://api.fastcrw.com
      CRW_API_KEY: YOUR_API_KEY
```

MCP tools in Continue only work in Agent mode, not plain chat mode.

## Gemini CLI

### Local embedded

```bash
gemini mcp add crw -- npx crw-mcp
```

Or edit `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

Useful commands:

- `gemini mcp list`
- `gemini mcp remove crw`

## osaurus

[osaurus](https://github.com/osaurus-ai/osaurus) is a native macOS agent harness. It does not use the standard `mcpServers` shape — it stores remote MCP servers in its own config and adds them as **Remote MCP Providers**.

### Add it from the UI (recommended)

Open Management (`⌘⇧M`) → **Providers** → **MCP Providers** → **Add MCP Provider**, then set:

| Field | Value |
|---|---|
| Name | `crw` |
| Transport | `stdio` |
| Execution host | `host` |
| Command | `npx` |
| Args | `crw-mcp` |

`crw_*` tools then appear in chat namespaced as `crw_scrape`, `crw_crawl`, `crw_check_crawl_status`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_cancel_extract`, `crw_parse_file` (and `crw_search` when a search backend is configured).

### Or edit the config file

Edit `~/.osaurus/providers/mcp.json` (the root is an object with a `providers` array):

```json
{
  "providers": [
    {
      "id": "11111111-1111-1111-1111-111111111111",
      "name": "crw",
      "url": "",
      "enabled": true,
      "transport": "stdio",
      "executionHost": "host",
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {},
      "authType": "none"
    }
  ]
}
```

:::note
`id` must be a valid UUID. Set `executionHost` to `host`, not `sandbox`: embedded `crw-mcp` auto-downloads and spawns LightPanda under `~/.crw` and needs the macOS host (the sandbox VM has no `npx`, network, or `~/.crw`).
:::

### fastcrw.com cloud

Add `CRW_API_URL` and your `CRW_API_KEY` as environment variables. Add `CRW_API_KEY` through the UI so osaurus stores it in the Keychain (listed under `secretEnvKeys`), rather than committing it to the JSON file:

```json
{
  "providers": [
    {
      "id": "11111111-1111-1111-1111-111111111111",
      "name": "crw",
      "url": "",
      "enabled": true,
      "transport": "stdio",
      "executionHost": "host",
      "command": "npx",
      "args": ["crw-mcp"],
      "env": { "CRW_API_URL": "https://api.fastcrw.com" },
      "secretEnvKeys": ["CRW_API_KEY"],
      "authType": "none"
    }
  ]
}
```

Cloud mode exposes `crw_search` (always-on managed search backend); local embedded mode shows it only when you configure a search backend.

## Any MCP client

If your client accepts the standard JSON `mcpServers` shape, start here:

### Local embedded

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"]
    }
  }
}
```

### fastcrw.com cloud

```json
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["crw-mcp"],
      "env": {
        "CRW_API_URL": "https://api.fastcrw.com",
        "CRW_API_KEY": "YOUR_API_KEY"
      }
    }
  }
}
```

## Local rendering and config

Local embedded mode uses the same config chain as `crw-server`:

- `config.default.toml`
- `config.local.toml`
- `CRW_...` environment overrides

If you need JavaScript rendering in local MCP mode, pass a renderer URL:

```bash
claude mcp add crw \
  -e CRW_RENDERER__LIGHTPANDA__WS_URL=ws://127.0.0.1:9222 \
  -- npx -y crw-mcp
```

Without a configured renderer, CRW still works in HTTP-only mode.

## How to choose local vs fastcrw.com

Use local embedded mode when:

- you want zero infrastructure,
- scrape/map/crawl are enough,
- or you want everything to stay on your own machine.

Use fastcrw.com when:

- you want managed infrastructure,
- you want the `crw_search` tool,
- or you do not want to manage a local renderer or server.

## What to read next

- [MCP Server](#mcp) for the mode overview
- [Search](#search) to see what cloud mode adds
- [JS Rendering](#js-rendering) if local embedded mode needs browser rendering
- [Authentication](#authentication) if you are using a self-hosted CRW server
