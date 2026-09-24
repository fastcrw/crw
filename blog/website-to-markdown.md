# How to Convert Websites to Clean Markdown for LLMs

> Turn any web page into clean, noise-free markdown ready for LLMs using CRW's scrape endpoint. No selectors, no regex.

**Published:** 2026-03-08  
**Updated:** 2026-03-08  
**Canonical:** https://fastcrw.com/blog/website-to-markdown

---

## The Problem with Raw HTML for LLMs

When you fetch a web page and pass the raw HTML to an LLM, you're wasting tokens — and money. A typical news article might be 800 words of actual content surrounded by 5,000+ tokens of navigation menus, sidebar widgets, cookie banners, script tags, tracking pixels, and footer links.

LLMs can work through the noise, but they do it at the cost of your token budget and sometimes at the cost of accuracy. The model has to "ignore" enormous amounts of irrelevant markup, which can cause it to miss details, misattribute content to the wrong section, or hallucinate when the signal-to-noise ratio gets too low.

## Why Markdown Beats HTML for LLMs

Markdown is a better format for LLM input for three reasons: token efficiency, structure preservation, and model familiarity.

**Token efficiency.** A raw HTML page with 800 words of content typically contains 6,000–12,000 tokens including markup, scripts, and styles. The same content as clean markdown is 900–1,200 tokens — an 80–90% reduction. At GPT-4o-mini rates, processing 10,000 pages drops from ~$10 to ~$1.

**Structure preservation.** Markdown preserves the document's semantic hierarchy: headings become `#` and `##` markers, lists stay as bullet points, code blocks are fenced with triple backticks, tables retain alignment. This structure helps the model locate specific sections and reason about document organization.

**Model familiarity.** LLMs are trained on massive amounts of markdown — GitHub READMEs, Stack Overflow answers, documentation sites. Models handle markdown natively and reliably. Raw HTML is also in training data but is treated as code to parse, not content to reason about.

| Input type | ~Tokens (800-word article) | Content ratio |
| --- | --- | --- |
| Raw HTML | 6,000–12,000 | 10–15% |
| Visible text only | 1,500–2,000 | 40–60% |
| CRW markdown | 900–1,200 | 85–95% |

## What CRW Strips and Why

CRW uses lol-html, a streaming HTML rewriter, to identify and remove non-content elements before markdown conversion. Here's what gets stripped and why:

- **` `, ` `, ` `** — site-wide navigation; irrelevant to page content
- **` `** — sidebars, related content widgets, ad slots
- **` `, ` `, ` `** — code and styling; not readable content
- **` `** — embedded third-party content, ad frames
- **` `** — inline icon markup; produces noisy output when converted
- **Banner and cookie notice patterns** — common class/id patterns (`.cookie-banner`, `#gdpr-popup`)

What CRW preserves:

  **Headings (``–``)** → markdown `#` hierarchy
  **Paragraphs, ``, ``** → body text
  **Lists (``, ``)** → `-` and `1.` markdown lists
  **Code blocks (`

`, ``)** → fenced code blocks with language hints **Tables** → markdown table syntax Links (``) → `[text](url)` if `links` format is requested Images (``) → alt text preserved as `![alt](src)` Basic Scrape to Markdown
```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "formats": ["markdown"]
  }'
```
 Response:
```
{
  "success": true,
  "data": {
    "markdown": "# Rust (programming language)\n\nRust is a multi-paradigm, general-purpose programming language...",
    "metadata": {
      "title": "Rust (programming language) - Wikipedia",
      "sourceURL": "https://en.wikipedia.org/wiki/Rust_(programming_language)"
    }
  }
}
```
 With TypeScript / Node.js
```
async function toMarkdown(url: string): Promise<string> {
  const res = await fetch("https://api.fastcrw.com/v1/scrape", { // or http://localhost:3000 for self-hosted
    method: "POST",
    headers: {
      "Authorization": "Bearer crw_live_YOUR_API_KEY",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ url, formats: ["markdown"] }),
  });
  const data = await res.json();
  if (!data.success) throw new Error(data.error);
  return data.data.markdown;
}

const markdown = await toMarkdown("https://docs.anthropic.com/en/api/overview");
console.log(markdown.substring(0, 500));
```
 With Python
```
import requests

def to_markdown(url: str) -> str:
    res = requests.post(
        "https://api.fastcrw.com/v1/scrape",  # or http://localhost:3000 for self-hosted
        headers={"Authorization": "Bearer crw_live_YOUR_API_KEY"},
        json={"url": url, "formats": ["markdown"]},
        timeout=30,
    )
    data = res.json()
    if not data["success"]:
        raise ValueError(data.get("error", "Scrape failed"))
    return data["data"]["markdown"]

md = to_markdown("https://docs.openai.com/api-reference/introduction")
print(md[:500])
```
 Handling Different Page Types News Articles News sites have heavy navigation and related-article widgets. CRW's `onlyMainContent` option focuses extraction on the article body specifically, using heuristics to identify the primary content area:
```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://techcrunch.com/some-article",
    "formats": ["markdown"],
    "onlyMainContent": true
  }'
```
 Documentation Pages Documentation often has left-rail navigation and right-rail "on this page" TOCs. Use `excludeTags` to remove them:
```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://docs.example.com/api-reference",
    "formats": ["markdown"],
    "excludeTags": [".sidebar", ".toc", "nav", "[data-testid=breadcrumb]"]
  }'
```
 E-Commerce Product Pages Product pages have structured data spread across multiple sections. Use `includeTags` to target only the product information you need:
```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://store.example.com/product/widget",
    "formats": ["markdown"],
    "includeTags": [".product-title", ".product-price", ".product-description", ".product-specs"]
  }'
```
 Blog Posts Blog posts typically work well with default settings, but `onlyMainContent: true` removes the comment section if you only want the article text:
```
{ "url": "https://blog.example.com/post", "formats": ["markdown"], "onlyMainContent": true }
```
 Advanced Format Options CRW exposes several options to fine-tune what gets extracted: `onlyMainContent` (boolean) — use heuristics to identify and extract only the primary content area, discarding sidebars and navigation `includeTags` (string[]) — CSS selectors; only include matched elements and their descendants `excludeTags` (string[]) — CSS selectors; remove matched elements before extraction `formats` — request `"markdown"`, `"html"`, `"links"`, or `"screenshot"` (needs a Chrome-class renderer tier) `waitFor` (number) — milliseconds to wait after page load before extracting (useful for JavaScript-rendered content) Batch Processing Multiple URLs
```
import PQueue from "p-queue";

async function batchToMarkdown(
  urls: string[],
  concurrency = 5,
): Promise<Map<string, string>> {
  const queue = new PQueue({ concurrency });
  const results = new Map<string, string>();

  await Promise.all(
    urls.map((url) =>
      queue.add(async () => {
        try {
          const md = await toMarkdown(url);
          results.set(url, md);
        } catch (err) {
          console.warn(`Failed: ${url}`, err);
        }
      }),
    ),
  );

  return results;
}

const pages = await batchToMarkdown([
  "https://docs.example.com/intro",
  "https://docs.example.com/authentication",
  "https://docs.example.com/endpoints",
  "https://docs.example.com/errors",
]);
console.log(`Fetched ${pages.size} pages`);
```
 Integrating with OpenAI
```
import OpenAI from "openai";
const openai = new OpenAI();

async function summarizeWithOpenAI(url: string) {
  const markdown = await toMarkdown(url);

  return openai.chat.completions.create({
    model: "gpt-4o-mini",
    messages: [
      { role: "system", content: "Summarize the article in 3 bullet points." },
      { role: "user", content: markdown },
    ],
    max_tokens: 300,
  }).then((r) => r.choices[0].message.content);
}
```
 Integrating with Anthropic
```
import Anthropic from "@anthropic-ai/sdk";
const anthropic = new Anthropic();

async function analyzeWithClaude(url: string, question: string) {
  const markdown = await toMarkdown(url);

  const message = await anthropic.messages.create({
    model: "claude-3-5-haiku-20241022",
    max_tokens: 1024,
    messages: [
      {
        role: "user",
        content: `Page content:

${markdown}

Question: ${question}`,
      },
    ],
  });

  return message.content[0].type === "text" ? message.content[0].text : "";
}
```
 Integrating with Ollama (Local LLMs)
```
async function summarizeWithOllama(url: string) {
  const markdown = await toMarkdown(url);

  const res = await fetch("http://localhost:11434/api/chat", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "llama3.2",
      messages: [
        { role: "system", content: "Summarize the following article concisely." },
        { role: "user", content: markdown },
      ],
      stream: false,
    }),
  });

  const data = await res.json();
  return data.message.content;
}
```
 Common Pitfalls Dynamic Content Not Loaded If a page uses JavaScript to render its main content and you get a skeleton or empty result, add a `waitFor` delay:
```
body: JSON.stringify({ url, formats: ["markdown"], waitFor: 2000 })
```
 Lazy-Loaded Images Images loaded lazily (via Intersection Observer) won't appear in the extracted content unless JavaScript rendering is enabled. For most RAG use cases this doesn't matter — you want text, not images. Paginated Content If the page uses infinite scroll or "load more" pagination, CRW will only capture the initially visible content. Use CRW's crawl endpoint with specific URL patterns to capture paginated pages individually. Login-Required Pages CRW can pass custom headers (cookies, Authorization) for authenticated pages:
```
body: JSON.stringify({
  url: "https://private.docs.com/api",
  formats: ["markdown"],
  headers: { "Authorization": "Bearer token", "Cookie": "session=xyz" },
})
```
 Aggressive Bot Detection Some sites block requests from known cloud IP ranges. If self-hosted CRW is getting blocked, fastCRW's proxy network rotates IPs automatically. Self-Host or Use fastCRW Cloud Self-Host for Free
```
docker run -p 3000:3000 ghcr.io/fastcrw/crw:latest
```
 Source: github.com/fastcrw/crw

### fastCRW Cloud

```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "formats": ["markdown"]}'
```

Sign up at [fastcrw.com](https://fastcrw.com) — 1000 free credits, no credit card.

## Frequently Asked Questions

### How does CRW convert HTML to markdown?

CRW uses lol-html, a streaming HTML rewriter, to process the page in a single pass. It removes non-content elements (nav, footer, scripts, ads), then converts semantic HTML elements to their markdown equivalents: `` becomes `#`, `` becomes `-`, `` becomes fenced code blocks, etc. No full DOM tree is built in memory, which keeps the process fast.

### Can CRW handle JavaScript-heavy pages?

Yes — CRW supports JavaScript rendering via LightPanda for pages that require it. Add `"waitFor": 2000` to give JavaScript time to execute. For most documentation and article pages, JavaScript rendering isn't needed and the default static fetch is faster. For complex SPAs requiring user interaction, Playwright-based scrapers may be more reliable.

### What's the maximum page size CRW can handle?

CRW processes HTML as a stream, so there's no hard maximum tied to available RAM. Very large pages (multi-megabyte HTML) will take longer but won't crash the process. For practical purposes, the bottleneck is network transfer time, not processing.

### How do I get just the main article content?

Use `"onlyMainContent": true` in your request. CRW applies content heuristics (similar to Mozilla's Readability) to identify and extract only the primary article area. You can also use `includeTags` to target specific CSS selectors if your target site has consistent markup.

### Is the markdown output clean enough for LLMs?

For most standard web pages, yes — the output is clean enough to pass directly to an LLM without further processing. The main exception is heavily formatted pages with complex nested tables or non-standard markup, where some manual cleanup might improve results. For RAG pipelines, the output embeds well because it's dense with content and low on noise.

### How do I handle pages that block scrapers?

Pass realistic browser headers (`User-Agent`, `Accept-Language`) in your request. For sites with aggressive bot detection, the self-hosted CRW may be blocked if it's running on a known cloud IP range — fastCRW's proxy network handles IP rotation automatically for those cases.
