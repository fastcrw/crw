# fastCRW Agent Skills

Reusable [agent skills](https://www.skills.sh) that teach AI coding agents how
to use **fastCRW** — the open-source, self-hostable Firecrawl alternative
(single Rust binary, ~14 MB RAM, Firecrawl-compatible `/v1` + `/v2` API, bundled
search backend, first-class MCP).

Works with Claude Code, Codex, Cursor, OpenCode, Gemini CLI, Windsurf, and any
agent the [`skills`](https://github.com/vercel-labs/skills) tool supports.

## Install

```bash
# All crw skills, into every detected agent:
npx skills add fastcrw/crw

# Just one skill:
npx skills add fastcrw/crw@crw-scrape

# Global (user-level) instead of project-level:
npx skills add -g fastcrw/crw
```

Or install as a plugin marketplace (Claude Code / Codex / Cursor) — the
`.claude-plugin/`, `.codex-plugin/`, and `.cursor-plugin/` manifests bundle the
skills plus the `crw-mcp` server (`.mcp.json`).

The skills drive the `crw` CLI, the `crw-mcp` MCP tools, or the REST API — see
each skill's Quick Start for all three call surfaces. No API key is needed for
self-hosted search; the managed `api.fastcrw.com` offers a free tier.

## Catalog

### Core / CLI — the verb ladder
Climb in order; stop at the cheapest rung that answers the need.

| Skill | What it does |
|-------|--------------|
| [`crw`](./crw/SKILL.md) | Hub — which verb to use, in what order. Start here. |
| [`crw-search`](./crw-search/SKILL.md) | Web search (own search backend, no API key). Step 1. |
| [`crw-scrape`](./crw-scrape/SKILL.md) | Single-page → markdown / HTML / JSON / links. Step 2. |
| [`crw-map`](./crw-map/SKILL.md) | Discover all URLs on a site (no content). Step 3. |
| [`crw-crawl`](./crw-crawl/SKILL.md) | Scrape many pages under a site/section. Step 4. |
| [`crw-parse`](./crw-parse/SKILL.md) | Parse local/remote PDF files → markdown/JSON. Step 5. |
| [`crw-extract`](./crw-extract/SKILL.md) | Typed JSON object from a page, against a schema. Step 6. |
| [`crw-watch`](./crw-watch/SKILL.md) | Change tracking / diffing — a crw-only primitive. Step 7. |

### Quality / meta
| Skill | What it does |
|-------|--------------|
| [`crw-dynamic-search`](./crw-dynamic-search/SKILL.md) | Filter raw web JSON in a subprocess so only the distilled answer reaches context. Biggest token-saver. |
| [`crw-best-practices`](./crw-best-practices/SKILL.md) | Verb selection, post-filtering stack, hybrid RAG, pitfalls. Reference. |

### Moat — what Firecrawl's skills can't offer
| Skill | What it does |
|-------|--------------|
| [`crw-migrate`](./crw-migrate/SKILL.md) | Coming from Firecrawl? Usually a one-line `base_url` swap. |
| [`crw-self-host`](./crw-self-host/SKILL.md) | Stand up your own crw + search backend + proxy pool. |

## Links

- Managed API: https://api.fastcrw.com · Docs: https://docs.fastcrw.com
- Source: https://github.com/fastcrw/crw
- License: AGPL-3.0
