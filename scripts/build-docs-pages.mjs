/**
 * Static docs page generator for docs.fastcrw.com
 *
 * Generates docs/{slug}/index.html for each sidebar entry so Google can
 * index each docs page as a separate URL instead of a single hash-based SPA.
 *
 * Usage:
 *   npm install marked
 *   node scripts/build-docs-pages.mjs
 */

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.join(__dirname, "..");
const DOCS_DIR = path.join(ROOT, "docs");
const CONTENT_DIR = path.join(DOCS_DIR, "docs");
const BASE_URL = "https://docs.fastcrw.com";

// ── Load config ──────────────────────────────────────────────────────────────
const { default: config } = await import(
  path.join(DOCS_DIR, "site.config.js")
);

// Flat list of { slug, title, priority } from sidebar
const slugMeta = config.sidebar.flatMap((section) =>
  section.children.map((child) => ({
    slug: child.slug,
    title: child.title,
    // Optional: a decision-intent title for search-result/OG tags, distinct
    // from the short sidebar label (`title`). Falls back to `title` below.
    seoTitle: child.seoTitle,
    sectionTitle: section.title,
  }))
);

// Slug → title lookup
const slugTitles = Object.fromEntries(slugMeta.map((m) => [m.slug, m.title]));

// Priority per section (higher for core endpoints)
const sectionPriority = {
  "Get Started": 0.9,
  "Core Endpoints": 0.85,
  "More APIs": 0.8,
  Integrations: 0.75,
  Deploy: 0.7,
  Reference: 0.6,
};

// ── Load index.html template ──────────────────────────────────────────────────
let template = await readFile(path.join(DOCS_DIR, "index.html"), "utf8");

// Make all local asset paths absolute (so subdir pages can load them)
template = template
  .replace(/href="css\//g, 'href="/css/')
  .replace(/href="js\//g, 'href="/js/')
  .replace(/href="site\.config\.js"/g, 'href="/site.config.js"')
  .replace(/src="js\//g, 'src="/js/')
  .replace(/src="logo/g, 'src="/logo')
  .replace(/src="favicon/g, 'src="/favicon')
  // Remove introduction.md preload (not needed for static pages)
  .replace(/\s*<link rel="preload" href="docs\/introduction\.md"[^>]*>\n?/g, "");

// ── Configure marked ──────────────────────────────────────────────────────────
marked.setOptions({ gfm: true, breaks: false });

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Strip HTML tags and return plain text */
function stripTags(html) {
  return html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
}

/** Extract description from page-subtitle or first <p> */
function extractDescription(mdContent) {
  const subtitleMatch = mdContent.match(
    /<p class="page-subtitle">([\s\S]*?)<\/p>/
  );
  if (subtitleMatch) return stripTags(subtitleMatch[1]).slice(0, 200);

  // Fallback: first paragraph of plain markdown
  const paraMatch = mdContent.match(/^(?!#|<|[-*]|\d\.)(.{40,})/m);
  if (paraMatch) return paraMatch[1].trim().slice(0, 200);

  return `${config.description || "CRW documentation"}.`;
}

/** GitHub-style slug for a heading's (possibly HTML) inner text */
function slugifyHeading(inner) {
  return inner
    .replace(/<[^>]+>/g, "") // strip inline tags (e.g. <code>)
    .replace(/&[a-z0-9#]+;/gi, "") // drop HTML entities (&amp; etc.)
    .toLowerCase()
    .trim()
    .replace(/[^\w\s-]/g, "") // drop punctuation
    .replace(/\s+/g, "-") // spaces → dashes
    .replace(/-+/g, "-") // collapse dashes
    .replace(/^-|-$/g, ""); // trim leading/trailing dashes
}

/** Add stable id="" anchors to headings (marked v18 emits none), so docs pages
 *  are deep-linkable per section. Headings that already carry an id are left
 *  untouched; duplicate slugs are disambiguated with a numeric suffix. */
function addHeadingIds(html) {
  const used = new Set();
  return html.replace(/<(h[1-6])>([\s\S]*?)<\/\1>/g, (match, tag, inner) => {
    let id = slugifyHeading(inner);
    if (!id) return match;
    const base = id;
    let n = 1;
    while (used.has(id)) id = `${base}-${n++}`;
    used.add(id);
    return `<${tag} id="${id}">${inner}</${tag}>`;
  });
}

/** Rewrite internal #slug links to /slug in rendered HTML */
function rewriteInternalLinks(html) {
  // href="#slug" → href="/slug" for known slugs
  return html.replace(/href="#([a-z][a-z0-9-]*)"/g, (match, slug) => {
    if (slugTitles[slug]) return `href="/${slug}"`;
    return match; // leave in-page anchors alone
  });
}

/** Build a page HTML from the template */
function buildPage(slug, title, description, content) {
  let page = template;

  // ── Head: per-page metadata ──
  page = page.replace(
    /<title>[^<]*<\/title>/,
    `<title>${escHtml(title)} — CRW Docs</title>`
  );
  page = page.replace(
    /<meta name="description" content="[^"]*">/,
    `<meta name="description" content="${escHtml(description)}">`
  );
  page = page.replace(
    /<link rel="canonical" href="[^"]*">/,
    `<link rel="canonical" href="${BASE_URL}/${slug}">`
  );

  // OG
  page = page.replace(
    /<meta property="og:url" content="[^"]*">/,
    `<meta property="og:url" content="${BASE_URL}/${slug}">`
  );
  page = page.replace(
    /<meta property="og:title" content="[^"]*">/,
    `<meta property="og:title" content="${escHtml(title)} — CRW | Web Scraper in Rust">`
  );
  page = page.replace(
    /<meta property="og:description" content="[^"]*">/,
    `<meta property="og:description" content="${escHtml(description)}">`
  );

  // Twitter
  page = page.replace(
    /<meta name="twitter:title" content="[^"]*">/,
    `<meta name="twitter:title" content="${escHtml(title)} — CRW Docs">`
  );
  page = page.replace(
    /<meta name="twitter:description" content="[^"]*">/,
    `<meta name="twitter:description" content="${escHtml(description)}">`
  );

  // ── Signup CTA attribution ──
  // The template's two /register CTAs are tagged for the docs home page; every
  // generated page rewrites them to its own slug. Referers from docs arrive
  // truncated to the origin (strict-origin-when-cross-origin), so without this
  // all 58 pages collapse into one row and we cannot tell which doc converts.
  page = page.split("ref=docs-home-").join(`ref=docs-${slug}-`);

  // ── Article: inject pre-rendered content ──
  page = page.replace(
    /<article id="article" role="main"><\/article>/,
    `<article id="article" role="main">${content}</article>`
  );

  // ── __INITIAL_SLUG__ before </body> ──
  page = page.replace(
    "</body>",
    `  <script>window.__INITIAL_SLUG__ = ${JSON.stringify(slug)};</script>\n</body>`
  );

  return page;
}

function escHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// ── Custom :::components (mirror docs/js/app.js parseComponents) ───────────────
// marked() doesn't understand our :::tabs / :::note / :::cards directives. The
// client (app.js) renders them, but for a prerendered page app.js TRUSTS the
// existing HTML and does NOT re-render (see its init() guard) — so if the
// prerender ships raw `:::tabs` text, that's what users AND crawlers see. We must
// emit the same markup here. Keep these regexes byte-identical to app.js.
// ponytail: getIcon returns "" — prerendered callout/card glyphs are dropped (the
// colored box + text still render). Port app.js's `icons` map here if glyphs matter.
const allSlugs = new Set(slugMeta.map((s) => s.slug));
const getIcon = () => "";

function normalizeDocHref(href) {
  if (!href) return href;
  if (/^(https?:|mailto:|tel:|#)/i.test(href)) return href;
  if (/^javascript:/i.test(href)) return "#";
  const [p, anchor] = href.split("#");
  const cleanPath = p
    .replace(/^\.?\//, "")
    .replace(/^docs\/docs\//, "")
    .replace(/^docs\//, "")
    .replace(/^\/docs\//, "")
    .replace(/\.md$/i, "")
    .replace(/\/$/, "");
  const slug = cleanPath.split("/").filter(Boolean).pop();
  if (!slug || !allSlugs.has(slug)) return href;
  return `/${slug}${anchor ? `#${anchor}` : ""}`;
}

function parseComponents(md) {
  // Cards: :::cards ... :::
  md = md.replace(/:::cards\n([\s\S]*?):::/g, (_, content) => {
    const cards = [];
    content.replace(/::card\{([^}]*)\}/g, (__, attrs) => {
      const props = {};
      attrs.replace(/(\w+)="([^"]*)"/g, (___, k, v) => { props[k] = v; });
      cards.push(props);
    });
    return `\n\n<div class="card-grid">${cards.map((c) =>
      `<a href="${normalizeDocHref(c.href || "#")}" class="doc-card"${c.href?.startsWith("http") ? ' target="_blank" rel="noopener"' : ""}>
        ${c.icon ? `<div class="doc-card-icon">${getIcon(c.icon)}</div>` : ""}
        <div class="doc-card-title">${c.title || ""}</div>
        <div class="doc-card-desc">${c.description || ""}</div>
      </a>`
    ).join("")}</div>\n\n`;
  });

  // Features: :::features ... :::
  md = md.replace(/:::features\n([\s\S]*?):::/g, (_, content) => {
    const items = [];
    content.replace(/::feature\{([^}]*)\}/g, (__, attrs) => {
      const props = {};
      attrs.replace(/(\w+)="([^"]*)"/g, (___, k, v) => { props[k] = v; });
      items.push(props);
    });
    return `\n\n<div class="feature-grid">${items.map((f) =>
      `<div class="feature-card">
        ${f.icon ? `<div class="feature-card-icon">${getIcon(f.icon)}</div>` : ""}
        <div class="feature-card-title">${f.title || ""}</div>
        <div class="feature-card-desc">${f.description || ""}</div>
      </div>`
    ).join("")}</div>\n\n`;
  });

  // Callouts: :::note/warning/tip/info ... :::
  md = md.replace(/:::(note|warning|tip|info)\n([\s\S]*?):::/g, (_, type, content) => {
    const iconName = type === "warning" ? "alert" : type === "tip" ? "check" : "info";
    return `\n\n<div class="callout callout-${type}"><div class="callout-icon">${getIcon(iconName)}</div><div class="callout-content">${content.trim()}</div></div>\n\n`;
  });

  // Collapsible: :::details{title="..."} ... :::
  md = md.replace(/:::details\{title="([^"]*)"\}\n([\s\S]*?):::/g, (_, title, content) => {
    return `\n\n<div class="details-block"><div class="details-summary" onclick="this.parentElement.classList.toggle('open')">${title}<span class="details-chevron">&#9654;</span></div><div class="details-content">${content.trim()}</div></div>\n\n`;
  });

  // Code tabs: :::tabs ... :::
  md = md.replace(/:::tabs\n([\s\S]*?):::/g, (_, content) => {
    const tabs = [];
    const tabRegex = /::tab\{title="([^"]*)"\}\n([\s\S]*?)(?=::tab\{|$)/g;
    let match;
    while ((match = tabRegex.exec(content)) !== null) {
      tabs.push({ title: match[1], content: match[2].trim() });
    }
    return `\n\n<div class="code-tabs">
      <div class="code-tabs-header">${tabs.map((t, i) =>
        `<button class="code-tab${i === 0 ? " active" : ""}" data-tab="${i}">${t.title}</button>`
      ).join("")}</div>
      ${tabs.map((t, i) =>
        `<div class="code-tab-panel${i === 0 ? " active" : ""}" data-panel="${i}">${t.content}</div>`
      ).join("")}
    </div>\n\n`;
  });

  return md;
}

/**
 * Render a doc the way app.js does: pull fenced code out first (so `:::` parsing
 * and tab content stay clean), expand our components into HTML, run marked over
 * the rest, then drop the code blocks back in.
 */
function renderMarkdown(md) {
  const codeBlocks = [];
  md = md.replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
    const langAttr = lang ? ` class="language-${lang}"` : "";
    const dataLang = lang ? ` data-lang="${lang}"` : "";
    const ph = `@@CB${codeBlocks.length}@@`;
    codeBlocks.push(`<pre${dataLang}><code${langAttr}>${escHtml(code.trim())}</code></pre>`);
    return `\n\n${ph}\n\n`;
  });
  let html = marked.parse(parseComponents(md));
  html = html
    .replace(/<p>\s*@@CB(\d+)@@\s*<\/p>/g, (_, i) => codeBlocks[i])
    .replace(/@@CB(\d+)@@/g, (_, i) => codeBlocks[i]);
  return html;
}

// ── Generate pages ────────────────────────────────────────────────────────────
let generated = 0;
const errors = [];
// slug → generated page HTML, reused by the legacy redirect stubs below
const pages = {};

for (const { slug, title, seoTitle } of slugMeta) {
  const mdPath = path.join(CONTENT_DIR, `${slug}.md`);

  if (!existsSync(mdPath)) {
    errors.push(`  MISSING: docs/docs/${slug}.md`);
    continue;
  }

  const mdContent = await readFile(mdPath, "utf8");
  const description = extractDescription(mdContent);

  // Convert markdown → HTML, expanding our :::components first (marked alone
  // leaves :::tabs/:::note/:::cards as raw text — see renderMarkdown).
  const rawHtml = renderMarkdown(mdContent);

  // Add per-heading id anchors, then rewrite #slug → /slug cross-references
  const content = rewriteInternalLinks(addHeadingIds(rawHtml));

  const pageHtml = buildPage(slug, seoTitle || title, description, content);
  pages[slug] = pageHtml;

  // Write to docs/{slug}/index.html
  const outDir = path.join(DOCS_DIR, slug);
  await mkdir(outDir, { recursive: true });
  await writeFile(path.join(outDir, "index.html"), pageHtml, "utf8");
  generated++;
}

// ── Generate redirect stubs for legacy URLs ───────────────────────────────────
// Google still has pre-flatten docs URLs on record (GSC "Not found (404)"):
//   • /docs/<slug>      — the old prefixed path; every page now lives at /<slug>
//   • /scrape, /crawl   — slugs renamed to /scraping, /crawling
//   • /v1/map           — legacy API-style path, now /map
// Emit a canonical meta-refresh stub at each old location so the 404s and any
// external backlinks resolve to the live page. Stubs are noindex.
// Coding agents fetch docs without running JS or following meta-refresh, so a
// bare "moved" stub costs them a second round trip. Each stub therefore serves
// the target page's own content; canonical + noindex keep it out of the index.
function buildRedirectStub(target) {
  const url = `${BASE_URL}/${target}`;
  const notice = `<p>This page moved to <a href="${url}">${url}</a>.</p>`;
  const page = pages[target];
  if (!page) {
    errors.push(`  REDIRECT TARGET NOT GENERATED: ${target}`);
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Redirecting…</title>
  <link rel="canonical" href="${url}">
  <meta name="robots" content="noindex, follow">
  <meta http-equiv="refresh" content="0; url=${url}">
  <script>location.replace(${JSON.stringify(url)});</script>
</head>
<body>${notice}</body>
</html>
`;
  }
  return page
    .replace(/\s*<meta name="robots" content="[^"]*">/, "")
    .replace(
      "</title>",
      `</title>\n  <meta name="robots" content="noindex, follow">\n  <meta http-equiv="refresh" content="0; url=${url}">`
    )
    .replace('<article id="article" role="main">', (m) => m + notice);
}

// old path (relative to docs/) → current slug it should resolve to
const legacyRedirects = {};
for (const { slug } of slugMeta) legacyRedirects[`docs/${slug}`] = slug;
const renamedSlugs = { scrape: "scraping", crawl: "crawling", "v1/map": "map" };
for (const [oldPath, target] of Object.entries(renamedSlugs)) {
  legacyRedirects[oldPath] = target;
  legacyRedirects[`docs/${oldPath}`] = target;
}

// Preserve the first-generation endpoint reference URLs. These pages were
// public and remain indexed/bookmarked; deleting them turns a docs cleanup
// into a breaking URL change. Point each one at its exact current reference
// page rather than collapsing every endpoint to the docs homepage.
Object.assign(legacyRedirects, {
  api: "rest-api",
  "api/scrape": "scraping",
  "api/crawl": "crawling",
  "api/map": "map",
  "api/search": "search",
  "api/extract": "extract",
  "api-reference": "rest-api",
  "api-reference/introduction": "rest-api",
  "api-reference/scrape": "scraping",
  "api-reference/crawl": "crawling",
  "api-reference/crawl/get-crawl-status": "crawling",
  "api-reference/crawl-url": "crawling",
  "api-reference/map": "map",
  "api-reference/endpoint/map": "map",
  "api-reference/search": "search",
  "api-reference/extract": "extract",
  // Early agent landing pages, previously hand-written and since gone stale.
  quickstart: "quick-start",
  "get-started/quickstart": "quick-start",
  "features/scrape": "scraping",
  "features/crawl": "crawling",
  "features/map": "map",
  "features/search": "search",
  "features/extract": "extract",
  // Paths coding agents guess when looking for the SDK.
  sdks: "sdk-reference",
  "sdks/python": "sdk-reference",
  "sdks/typescript": "sdk-reference",
  "integrations/sdk-examples": "sdk-examples",
});

let redirectsWritten = 0;
for (const [oldPath, target] of Object.entries(legacyRedirects)) {
  const outDir = path.join(DOCS_DIR, oldPath);
  // Never clobber a real generated page (slugMeta dirs live at docs/<slug>/).
  if (slugTitles[oldPath] && !(oldPath in renamedSlugs)) continue;
  await mkdir(outDir, { recursive: true });
  await writeFile(path.join(outDir, "index.html"), buildRedirectStub(target), "utf8");
  redirectsWritten++;
}

// ── Regenerate sitemap.xml ────────────────────────────────────────────────────
const now = new Date().toISOString().split("T")[0];

const urlEntries = [
  `  <url><loc>${BASE_URL}/</loc><priority>1.0</priority><changefreq>weekly</changefreq><lastmod>${now}</lastmod></url>`,
  ...slugMeta.map(({ slug, sectionTitle }) => {
    const priority = sectionPriority[sectionTitle] ?? 0.6;
    return `  <url><loc>${BASE_URL}/${slug}</loc><priority>${priority}</priority><changefreq>monthly</changefreq><lastmod>${now}</lastmod></url>`;
  }),
];

const sitemap = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urlEntries.join("\n")}
</urlset>
`;

await writeFile(path.join(DOCS_DIR, "sitemap.xml"), sitemap, "utf8");

// ── Summary ───────────────────────────────────────────────────────────────────
console.log(`\n✓ Generated ${generated}/${slugMeta.length} static pages`);
console.log(`✓ Wrote ${redirectsWritten} legacy redirect stubs`);
console.log(`✓ Updated docs/sitemap.xml (${slugMeta.length + 1} URLs)`);
if (errors.length) {
  console.warn(`\nWarnings:`);
  errors.forEach((e) => console.warn(e));
}
