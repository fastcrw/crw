# CRW v0.0.8: Wikipedia Fix, BYOK Extraction, and Smarter Noise Detection

> CRW v0.0.8 fixes Wikipedia extraction with onlyMainContent, adds bring-your-own-key LLM extraction, introduces 3-tier noise matching, and hardens the content cleaning pipeline.

**Published:** 2026-04-02  
**Updated:** 2026-04-02  
**Canonical:** https://fastcrw.com/blog/crw-v0-0-8-release

---

CRW v0.0.8 fixes a significant content extraction bug that affected Wikipedia and other MediaWiki sites, adds bring-your-own-key LLM extraction for structured data, and introduces a three-tier noise pattern matching system that reduces false positives in content cleaning.

## The Wikipedia Bug

This was the kind of bug that's embarrassing in hindsight but subtle to find.

When you scraped a Wikipedia page with `onlyMainContent: true`, CRW returned an empty or near-empty result. The article body was being stripped entirely. A scraper that can't handle Wikipedia is a scraper with a credibility problem.

### What Happened

CRW's noise detection works by scanning element classes and IDs for patterns associated with non-content elements: `sidebar`, `footer`, `nav`, `toc`, `social`, `comment`. The word `toc` (table of contents) was matched as a substring.

Wikipedia's `` element has `class="client-js vector-feature-toc-pinned-clientpref-1 vector-toc-available"`. The substring `toc` in `vector-toc-available` matched the noise pattern — so CRW removed the `` element itself, which means it removed everything.

The fix has two parts:

### Three-Tier Noise Pattern Matching

Instead of one-size-fits-all substring matching, v0.0.8 uses three tiers:

1. **Substring matching** for long, unambiguous patterns: `sidebar`, `footer`, `navigation`, `advertisement`. These are unlikely to appear as substrings of unrelated class names.
2. **Exact token matching** for short, ambiguous patterns: `toc`, `share`, `social`, `comment`, `related`. The class string is split on whitespace and hyphens, and each token is matched exactly. `vector-toc-available` splits into `[vector, toc, available]` — `toc` matches as a token, but only if it's the element's primary purpose, not a feature flag.
3. **Prefix matching** for ad-related patterns: `ad-`, `ads-`. This catches `ad-container` and `ads-wrapper` without matching `address` or `adapter`.

### Structural Element Guard

Even with smarter matching, CRW v0.0.8 now has a hard guard: ``, ``, ``, and `` elements are never removed by the noise handler, regardless of their class names. These are structural elements — removing them is always wrong.

### Re-Clean After Readability

Wikipedia articles have nested noise elements (infoboxes, navigation boxes, category links) that survive the initial cleaning pass because they're inside the readability-selected content container. v0.0.8 runs a second cleaning pass after readability extraction to catch these residual elements.

The result: Wikipedia pages now extract correctly with `onlyMainContent: true`, producing about 49% less content than a full page scrape — which is the right behavior. The infobox, TOC, navigation, and category links are stripped; the article body is preserved.

## BYOK LLM Extraction

Before v0.0.8, structured extraction with `formats: ["json"]` required configuring an LLM provider in CRW's server config. This meant self-hosters had to set API keys in their deployment config, which creates problems for multi-tenant setups and makes it harder to switch providers per request.

v0.0.8 adds per-request LLM configuration:

```
curl -X POST https://api.fastcrw.com/v1/scrape \
  -H "Authorization: Bearer crw_live_YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://store.example.com/product/widget",
    "formats": ["json"],
    "jsonSchema": {
      "type": "object",
      "properties": {
        "name": { "type": "string" },
        "price": { "type": "number" },
        "currency": { "type": "string" },
        "in_stock": { "type": "boolean" }
      }
    },
    "llmProvider": "openai",
    "llmModel": "gpt-4o-mini",
    "llmApiKey": "sk-your-key-here"
  }'
```

The `llmProvider`, `llmModel`, and `llmApiKey` fields override the server-level config for that single request. This enables several workflows:

- **Multi-tenant platforms** — each user provides their own API key, so LLM costs are borne by the user, not the platform
- **Provider switching** — use GPT-4o-mini for simple extractions and Claude for complex ones, in the same CRW instance
- **Testing** — try different models against the same page without restarting CRW

If `formats: ["json"]` is requested without a `jsonSchema`, CRW now returns a 400 error with a clear message instead of silently falling back to markdown. This prevents the common mistake of expecting structured output without providing a schema.

## Other Fixes

### Block Detection Skip for Large Pages

CRW's anti-bot detection checks response content for interstitial patterns (CAPTCHA forms, challenge pages). On large pages (>50 KB), this check was causing false positives — Wikipedia's 200 KB HTML occasionally matched patterns that looked like bot challenges. v0.0.8 skips interstitial detection for responses larger than 50 KB, since real bot challenge pages are always small.

### Null Byte URL Rejection

URLs containing `%00` or raw null bytes are now rejected at the validation layer. These can cause issues in downstream processing (file system operations, logging) and are never valid in HTTP URLs.

### Timeout Increase

Default request timeout increased from 60s to 120s. Complex pages with JavaScript rendering and multiple redirects were hitting the 60s limit on slower connections. 120s provides more headroom without allowing indefinite hangs.

## Upgrade

```
# Docker
docker pull ghcr.io/fastcrw/crw:0.0.8

# Cargo
cargo install crw-server
```

Backward-compatible with all previous versions. All new fields (`llmProvider`, `llmModel`, `llmApiKey`) are optional — existing API calls work unchanged.

For the full changelog, see [CHANGELOG.md](https://github.com/fastcrw/crw/blob/main/CHANGELOG.md).
