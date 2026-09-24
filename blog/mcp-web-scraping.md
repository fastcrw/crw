# How to Expose Web Scraping to AI Agents with MCP

> Connect CRW's built-in MCP server to Claude, Cursor, or any MCP-compatible AI agent for live web scraping in agentic workflows.

**Published:** 2026-03-07  
**Updated:** 2026-03-07  
**Canonical:** https://fastcrw.com/blog/mcp-web-scraping

---

## What Is MCP and Why Does It Matter for Scraping?

The Model Context Protocol (MCP) is an open standard that lets AI models call external tools in a structured, auditable way. Instead of asking your LLM to write fetch calls and parse HTML itself (unreliably), you expose tools through MCP and the model calls them directly.

For web scraping, this means your AI agent can say "scrape this URL" and get back clean, structured content — without you writing prompt engineering hacks to extract data from raw HTML responses.

CRW ships with a built-in MCP server. There's no separate package to install or configure. The same binary that runs your REST API also runs the MCP server.

## What MCP Actually Does (Technical)

MCP runs as a JSON-RPC 2.0 protocol over stdio. When a client (Claude Desktop, Cursor, or your own code) starts an MCP server process, it communicates by writing JSON-RPC messages to the process's stdin and reading responses from stdout. There's no HTTP server involved — it's direct process communication.

The MCP lifecycle looks like this:

1. **Initialize** — the client sends an `initialize` request; the server responds with its name, version, and capabilities
2. **List tools** — the client sends `tools/list`; the server responds with the available tools, their input schemas, and descriptions
3. **Call tool** — the client sends `tools/call` with the tool name and arguments; the server executes the tool and returns the result

Each tool has a JSON Schema that describes its input parameters. The AI model uses this schema to know what arguments to provide. CRW's tools expose the same parameters as its REST API — `url`, `formats`, `limit`, etc. — so the tool description is self-documenting.

A raw MCP exchange for a scrape call looks like:

```
// Client sends:
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "crw_scrape",
    "arguments": {
      "url": "https://news.ycombinator.com",
      "formats": ["markdown"]
    }
  }
}

// Server responds:
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "# Hacker News\n\n## Top Stories\n..."
      }
    ]
  }
}
```

The AI client receives the `text` content and injects it into the model's context window. From the model's perspective, it called a tool and got back a markdown string — exactly what it would need to reason about the page's contents.

## What Tools Does CRW's MCP Server Expose?

Nine tools:

- **crw_scrape** — Fetches a single URL and returns clean markdown (and optionally HTML, links, or structured JSON)
- **crw_crawl** — Crawls a site up to a page limit, returning markdown for each discovered page
- **crw_check_crawl_status** — Checks the status of an async crawl job
- **crw_map** — Returns all URLs found on a site, useful for site discovery without full content extraction
- **crw_extract** — Starts an async multi-URL structured-extraction job
- **crw_check_extract_status** — Checks an async extract job and returns its per-URL results
- **crw_cancel_extract** — Idempotently cancels an extract job and returns its canonical status
- **crw_search** — Searches the web and returns content from matching pages
- **crw_parse_file** — Parses a file (including PDFs) and returns its content as markdown

These map directly to CRW's REST endpoints, so the behavior is identical whether you call over HTTP or through MCP. The tool descriptions and parameter schemas are written to help the AI understand when to use each one — for example, the `crw_map` tool description explains that it's faster than `crw_crawl` for URL discovery.

## CRW MCP vs the crw-mcp npm Package

There are two different ways to use CRW over MCP, and the distinction matters:

### Option A: Local Binary (`crw-mcp`)

If you self-host CRW, the `crw-mcp` binary starts the MCP server backed by your local CRW instance. The binary handles all scraping directly — no API key required, no network calls to external services. Your config points at the local binary:

```
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

This is the zero-cost, fully private option. All scraping happens on your own machine or server.

### Option B: npm Package (`npx -y crw-mcp`)

The `crw-mcp` npm package is a thin MCP wrapper that proxies tool calls to the fastCRW cloud API. It is installed and run via npx as `crw-mcp`. This gives you access to fastCRW's proxy network and managed infrastructure:

```
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["-y", "crw-mcp"],
      "env": {
        "FASTCRW_API_KEY": "your-api-key"
      }
    }
  }
}
```

This is the right choice when you want better success rates on bot-protected sites, don't want to manage a local binary, or are using CRW from a machine that can't run Docker (some locked-down corporate environments).

The tool interface is identical between both options — the same nine tools with the same parameters. Switching between them is a config-file change.

## Option 1: Connect to Claude Desktop

First, make sure you have the CRW MCP binary installed. Install it via npm:

```
npm install -g crw-mcp
```

Then add this to your Claude Desktop config file (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows):

```
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

Restart Claude Desktop. In the tool menu you should now see `crw` as an available server with tools including `crw_scrape`, `crw_crawl`, `crw_map`, `crw_extract`, `crw_check_extract_status`, `crw_search`, `crw_check_crawl_status`, and `crw_parse_file`.

Now you can tell Claude: *"Scrape https://docs.example.com/api and summarize the authentication section."* Claude will call `scrape`, get the markdown, and answer based on the actual page content — not its training data cutoff.

## Option 2: Connect to Cursor

In Cursor's settings, navigate to **MCP Servers** and add:

```
{
  "name": "crw",
  "command": "crw-mcp"
}
```

Once enabled, Cursor's AI can use `scrape`, `crawl`, `map`, and other CRW tools when responding to your prompts. This is particularly useful for coding tasks — "scrape the API docs for this library and show me how to authenticate" becomes a real-time web lookup rather than a retrieval from the model's training data.

A common pattern in Cursor is using `map` first to discover what documentation pages exist, then selectively `scrape`-ing only the relevant ones rather than crawling everything.

## Option 3: Use the Docker-Based MCP Server

If you prefer not to install the binary directly, run CRW as a Docker container and connect via stdio transport:

```
{
  "mcpServers": {
    "crw": {
      "command": "docker",
      "args": ["run", "--rm", "-i", "ghcr.io/fastcrw/crw:latest", "crw-mcp"]
    }
  }
}
```

This spins up a CRW container for each MCP session. Slightly slower to start (~1–2 seconds for Docker to start the container) but requires no local binary install. Useful for team environments where a Docker Desktop installation is the common denominator.

## Option 4: Programmatic MCP Client

If you're building your own AI agent and want to call CRW's MCP tools programmatically, use the MCP TypeScript SDK:

```
import { Client } from "@modelcontextprotocol/sdk/client/index.js";

const transport = new StdioClientTransport({
  command: "crw-mcp",
});

const client = new Client({ name: "my-agent", version: "1.0.0" }, {});
await client.connect(transport);

// Scrape a page
const result = await client.callTool({
  name: "crw_scrape",
  arguments: {
    url: "https://news.ycombinator.com",
    formats: ["markdown"],
  },
});

console.log(result.content[0].text); // Clean markdown of HN front page

// Map a site's URLs before deciding what to crawl
const mapResult = await client.callTool({
  name: "crw_map",
  arguments: { url: "https://docs.example.com" },
});
console.log(mapResult.content[0].text); // JSON array of URLs

await client.close();
```

The same pattern works with the Python MCP SDK:

```
import asyncio
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

async def main():
    server_params = StdioServerParameters(
        command="crw-mcp",
    )
    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            result = await session.call_tool(
                "crw_scrape",
                arguments={
                    "url": "https://example.com",
                    "formats": ["markdown"],
                },
            )
            print(result.content[0].text)

asyncio.run(main())
```

## Advanced MCP Patterns

### Map Then Scrape: Targeted Extraction

For large documentation sites, use `map` to discover all URLs, filter to the relevant subset, then scrape only what you need:

```
// Step 1: discover all URLs on the docs site
const mapResult = await client.callTool({
  name: "crw_map",
  arguments: { url: "https://docs.stripe.com/api" },
});

const allUrls: string[] = JSON.parse(mapResult.content[0].text);

// Step 2: filter to pages relevant to the task
const paymentUrls = allUrls.filter(url =>
  url.includes("/payments") || url.includes("/charges")
);

// Step 3: scrape only the relevant pages
for (const url of paymentUrls.slice(0, 10)) {
  const page = await client.callTool({
    name: "crw_scrape",
    arguments: { url, formats: ["markdown"] },
  });
  // process page.content[0].text
}
```

### Multi-Step Research Agent

Chain calls to build a research pipeline: map a site, scrape high-value pages, and synthesize a report. In a LangChain or custom agent loop:

```
// Pseudo-code agent loop using CRW MCP tools
const task = "Summarize the key differences between Stripe and PayPal's pricing";

// Agent calls map on both sites
const stripeMap = await callTool("crw_map", { url: "https://stripe.com/pricing" });
const paypalMap = await callTool("crw_map", { url: "https://www.paypal.com/us/webapps/mpp/merchant-fees" });

// Agent scrapes the relevant pricing pages
const stripePricing = await callTool("crw_scrape", {
  url: "https://stripe.com/pricing",
  formats: ["markdown"],
});
const paypalPricing = await callTool("crw_scrape", {
  url: "https://www.paypal.com/us/webapps/mpp/merchant-fees",
  formats: ["markdown"],
});

// Agent synthesizes: feeds both markdown strings to LLM for comparison
```

### Incremental Site Monitoring

Use `map` to detect new pages, then `scrape` only the deltas:

```
// Store known URLs
const knownUrls = new Set(await loadFromStorage());

// Get current site map
const current = await client.callTool({
  name: "crw_map",
  arguments: { url: "https://competitor.com/blog" },
});
const currentUrls = JSON.parse(current.content[0].text);

// Find new pages
const newUrls = currentUrls.filter(url => !knownUrls.has(url));

// Scrape only new content
for (const url of newUrls) {
  const content = await client.callTool({
    name: "crw_scrape",
    arguments: { url, formats: ["markdown"] },
  });
  await processNewContent(url, content.content[0].text);
  knownUrls.add(url);
}
await saveToStorage([...knownUrls]);
```

## Agentic Use Cases

### 1. Competitive Monitoring Agent

An agent that runs nightly to check competitor pricing, feature pages, and blog posts for updates. It uses `map` to detect new pages, `scrape` to fetch changed content, and a diff algorithm to identify what changed. The agent then generates a Slack message summarizing the week's competitor movements.

### 2. Documentation QA Agent

An agent embedded in a support workflow. When a customer asks a technical question, the agent uses `scrape` to fetch the relevant documentation page in real time, ensuring its answer reflects the current docs rather than potentially stale training data. This is especially valuable for fast-moving developer products where docs change frequently.

### 3. Research Assistant

An agent that accepts a research question, uses `map` to discover relevant pages across a set of source sites (arXiv, GitHub, documentation), then `scrape`s the top candidates and synthesizes a structured summary. The key advantage over a web search approach is that the agent gets full page content, not just snippets.

### 4. Price Tracking Agent

An agent that monitors product pricing pages across multiple e-commerce sites. It scrapes target pages on a schedule, parses price data from the markdown output, and alerts when prices cross a threshold. Because CRW returns clean markdown, the price extraction regex or LLM prompt is much simpler than parsing raw HTML.

### 5. Code Review Assistant

An agent in Cursor that, given a library name, automatically scrapes its changelog, README, and API docs to help answer "what changed in v3?" or "how do I use this new method?". The MCP integration means the agent can fetch this information mid-conversation without leaving the editor.

## MCP vs REST: When to Use Each

Both interfaces expose the same underlying CRW functionality. The choice depends on your context:

| Scenario | Better fit | Why |
| --- | --- | --- |
| AI agent (Claude, Cursor, GPT) | MCP | Native tool calling — no HTTP boilerplate in agent code |
| Backend automation script | REST | Standard HTTP — easier to integrate with any language or framework |
| RAG pipeline batch indexing | REST | Easier to control concurrency, retries, and progress tracking |
| Interactive research in IDE | MCP | Real-time tool calls within the editor context |
| Scheduled cron job | REST | Simpler to call from cron scripts or workflow tools |
| Custom LLM agent framework | Either | REST if the framework has no MCP support; MCP if it does |

## Troubleshooting MCP Connection

### Binary Not Found

If your MCP client says the server failed to start, the most common cause is the `crw-mcp` binary not being on the `PATH`. MCP clients start the command in a clean shell environment that may not have your user PATH.

Fix: use the full path to the binary:

```
{
  "mcpServers": {
    "crw": {
      "command": "/usr/local/bin/crw-mcp"
    }
  }
}
```

Find the full path with: `which crw-mcp`

### JSON Config Errors

Claude Desktop and Cursor both silently ignore MCP servers if the JSON config file has a syntax error. Always validate your config JSON before restarting:

```
cat ~/Library/Application Support/Claude/claude_desktop_config.json | python3 -m json.tool
```

If the command prints the JSON, the syntax is valid. If it prints an error, find and fix the syntax issue (typically a trailing comma or missing quote).

### Permission Issues on macOS

On macOS, you may need to grant the MCP client permission to run the `crw-mcp` binary. If you see a "cannot be opened because the developer cannot be verified" dialog, run:

```
xattr -d com.apple.quarantine /usr/local/bin/crw-mcp
```

### Docker Transport Not Working

When using the Docker transport, ensure Docker Desktop is running before launching Claude Desktop or Cursor. The MCP client starts Docker as a subprocess — if Docker isn't running, the server will fail to start with a cryptic error. Test manually first:

```
docker run --rm -i ghcr.io/fastcrw/crw:latest crw-mcp
# Should start without error and wait for stdin input
```

### Debugging MCP Exchanges

To see the raw JSON-RPC messages exchanged, run `crw-mcp` with verbose logging:

```
CRW_LOG=debug crw-mcp
```

This prints each incoming request and outgoing response to stderr, which most MCP clients log to a debug console.

## Real-World Agentic Workflow Example

Here's a typical pattern for an agent that monitors competitor documentation:

1. Agent receives task: "Check if the competitor updated their pricing page"
2. Agent calls `scrape` on the competitor's pricing URL
3. Agent compares the returned markdown to a stored snapshot
4. Agent generates a summary of what changed

Without MCP, you'd need to write API call boilerplate, handle authentication, parse responses, and pipe the content back into the LLM context manually. With MCP, the agent handles all of this natively — you describe the task in natural language and the model handles the tool orchestration.

## Try It Yourself

### Self-Host for Free

Install the CRW MCP binary and configure your MCP client in minutes:

```
npm install -g crw-mcp
# Then add to your claude_desktop_config.json
```

Source and docs: [github.com/fastcrw/crw](https://github.com/fastcrw/crw)

### Use fastCRW's Cloud MCP

No binary to install — use the npm package backed by fastCRW's proxy network:

```
{
  "mcpServers": {
    "crw": {
      "command": "npx",
      "args": ["-y", "crw-mcp"],
      "env": {
        "FASTCRW_API_KEY": "your-api-key"
      }
    }
  }
}
```

Sign up at [fastcrw.com](https://fastcrw.com) — 1000 free credits, no credit card required.

## Tips for Effective MCP Scraping

- **Use `map` before `crawl`** — Map gives you the URL list so your agent can decide which pages are worth scraping before committing to a full crawl.
- **Set page limits on crawl** — Large sites can have thousands of pages. Always set a `limit` parameter to prevent runaway crawls.
- **Request only the formats you need** — Requesting just `markdown` is faster than requesting markdown + HTML + links.
- **Cache aggressively** — If your agent re-visits the same URLs, add a caching layer. CRW doesn't cache by default.
- **Use descriptive prompts** — When asking an AI agent to use MCP tools, be specific about which URL to scrape. Vague requests may result in the model choosing a URL based on its training data rather than calling `map` first.

## Frequently Asked Questions

### What is MCP web scraping?

MCP web scraping means exposing a web scraper as a tool through the Model Context Protocol, so AI agents (Claude, Cursor, GPT-based agents) can call it directly. Instead of writing HTTP integration code in your agent, you configure an MCP server and the AI model calls `scrape`, `crawl`, or `map` as native tool calls. The model gets back clean content — typically markdown — ready to reason about.

### Which AI tools support MCP?

Claude Desktop and Claude's API with tool use support MCP natively. Cursor supports MCP for its in-editor AI. Any AI agent framework that implements the MCP client spec can also use MCP servers — including custom agents built with the MCP TypeScript or Python SDKs. The MCP ecosystem is growing quickly; new integrations are added regularly.

### Does CRW's MCP server require an API key?

The self-hosted binary (`crw-mcp`) does not require an API key by default — it scrapes directly without any authentication. If you set `CRW_API_KEY` on your CRW server, the MCP server will use it automatically. The `crw-mcp` npm package requires a fastCRW API key set as the `FASTCRW_API_KEY` environment variable.

### What tools does CRW expose via MCP?

Nine tools: **crw_scrape** (fetch a single URL and return markdown, HTML, or links), **crw_crawl** (crawl a site up to a page limit, returning markdown for each page), **crw_check_crawl_status** (check an async crawl job), **crw_map** (return all URLs found on a site without fetching full content), **crw_extract** (async multi-URL structured extraction), **crw_check_extract_status** (poll an extract job), **crw_cancel_extract** (cancel an extract job), **crw_search** (search the web and return content from matching pages), and **crw_parse_file** (parse files including PDFs to markdown). These correspond directly to CRW's REST endpoints.

### How do I debug MCP connection issues?

Start with the JSON config file syntax — use `python3 -m json.tool` to validate it. Then check that the binary path is correct and accessible. Run `crw-mcp` manually in your terminal to verify it starts without error. For deeper debugging, set `CRW_LOG=debug` to see the raw JSON-RPC exchanges. Claude Desktop logs MCP errors to `~/Library/Logs/Claude/` on macOS.

### Can I use CRW's MCP server with my own AI agent?

Yes — use the [MCP TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk) or [Python SDK](https://github.com/modelcontextprotocol/python-sdk) to connect to `crw-mcp` as a subprocess. Your agent code starts the `crw-mcp` process, connects via stdio transport, and calls tools using the standard MCP client API. See the programmatic client example above for working code.

### Is there a rate limit on the self-hosted MCP server?

The self-hosted `crw-mcp` has no built-in rate limiting — it's limited only by your server's network and CPU capacity. If you're running CRW on a shared server, you can add rate limiting via the `CRW_RATE_LIMIT` environment variable or via a reverse proxy like Nginx. fastCRW's cloud MCP applies per-plan rate limits.
