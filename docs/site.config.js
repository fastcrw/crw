export default {
  name: "CRW",
  description: "Open-source Firecrawl alternative — self-hosted web scraper & crawler in Rust with MCP server for AI agents",
  logo: "/logo-icon.svg",
  logoDark: "/logo-dark.svg",
  author: {
    name: "us",
    url: "https://usxperiments.com",
  },

  navLinks: [
    { label: "Docs", href: "/introduction" },
    { label: "API", href: "/rest-api" },
    { label: "MCP", href: "/mcp" },
    { label: "GitHub", href: "https://github.com/fastcrw/crw", external: true },
  ],

  navTabs: [
    {
      label: "Start",
      href: "/quick-start",
      match: ["introduction", "quick-start", "migrate-from-firecrawl", "choose-endpoint", "authentication", "playground", "rest-api", "installation"],
    },
    {
      label: "Endpoints",
      href: "/scraping",
      match: ["scraping", "search", "map", "crawling", "extract", "pdf-parsing"],
    },
    {
      label: "MCP",
      href: "/mcp",
      match: ["mcp", "mcp-clients", "crw-browse", "sdk-reference", "sdk-examples", "integrations", "agent-onboarding"],
    },
    {
      label: "Guides",
      href: "/recipe-rag",
      match: ["recipe-rag", "recipe-mcp-5min", "recipe-monitoring", "recipe-js-spa", "recipe-batch", "recipe-product-catalog", "recipe-pdf", "troubleshooting"],
    },
    {
      label: "Self-Host",
      href: "/self-hosting",
      match: ["self-hosting", "docker", "configuration", "self-hosting-hardening", "js-rendering", "proxies"],
    },
    {
      label: "Reference",
      href: "/response-shapes",
      match: ["response-shapes", "output-formats", "rate-limits", "error-codes", "credit-costs", "compatibility", "v2-api", "capabilities", "glossary", "changelog", "architecture", "crates"],
    },
  ],

  quickLinks: [
    { title: "Playground", href: "https://fastcrw.com/playground", icon: "play", external: true },
    { title: "Get API Key", href: "https://fastcrw.com/register", icon: "key", external: true },
    { title: "GitHub", href: "https://github.com/fastcrw/crw", icon: "github", external: true },
    { title: "Changelog", href: "/changelog", icon: "list" },
  ],

  sidebar: [
    {
      title: "Get Started",
      children: [
        { title: "Introduction", slug: "introduction", icon: "rocket", seoTitle: "Web Scraping, Search & Crawl API" },
        { title: "Quick Start", slug: "quick-start", icon: "play" },
        { title: "Migrate from Firecrawl", slug: "migrate-from-firecrawl", icon: "git-branch" },
        { title: "Choose Your Endpoint", slug: "choose-endpoint", icon: "git-branch" },
        { title: "Authentication", slug: "authentication", icon: "key" },
        { title: "API Playground", slug: "playground", icon: "play" },
        { title: "API Overview", slug: "rest-api", icon: "server" },
        { title: "Installation", slug: "installation", icon: "box", seoTitle: "Install: CLI, Docker, or Self-Host" },
      ],
    },
    {
      title: "Core Endpoints",
      children: [
        { title: "Scrape", slug: "scraping", icon: "code" },
        { title: "Search", slug: "search", icon: "search" },
        { title: "Research API", slug: "research-api", icon: "book-open" },
      ],
    },
    {
      title: "More APIs",
      children: [
        { title: "Map", slug: "map", icon: "map" },
        { title: "Crawl", slug: "crawling", icon: "globe" },
        { title: "Extract", slug: "extract", icon: "zap" },
        { title: "PDF Parsing", slug: "pdf-parsing", icon: "file-text" },
        { title: "Monitoring", slug: "monitoring", icon: "bell" },
      ],
    },
    {
      title: "Integrations",
      children: [
        { title: "MCP Server", slug: "mcp", icon: "plug" },
        { title: "MCP Client Setup", slug: "mcp-clients", icon: "settings" },
        { title: "Browser Automation (crw-browse)", slug: "crw-browse", icon: "globe" },
        { title: "SDK Reference", slug: "sdk-reference", icon: "book" },
        { title: "SDK Examples", slug: "sdk-examples", icon: "code" },
        { title: "Framework Integrations", slug: "integrations", icon: "layers" },
        { title: "Agent Onboarding", slug: "agent-onboarding", icon: "book" },
      ],
    },
    {
      title: "Guides & Recipes",
      children: [
        { title: "Build a RAG Knowledge Base", slug: "recipe-rag", icon: "book" },
        { title: "Claude Web Access in 5 Min", slug: "recipe-mcp-5min", icon: "zap" },
        { title: "Monitor a Page → Slack", slug: "recipe-monitoring", icon: "bell" },
        { title: "Scrape a JS-Heavy SPA", slug: "recipe-js-spa", icon: "code" },
        { title: "Batch-Scrape a URL List", slug: "recipe-batch", icon: "layers" },
        { title: "Product Catalog Extraction", slug: "recipe-product-catalog", icon: "zap" },
        { title: "Parse PDF Reports", slug: "recipe-pdf", icon: "file-text" },
        { title: "Troubleshooting / FAQ", slug: "troubleshooting", icon: "info" },
      ],
    },
    {
      title: "Deploy",
      children: [
        { title: "Self-Hosting", slug: "self-hosting", icon: "server" },
        { title: "Docker", slug: "docker", icon: "box" },
        { title: "Configuration", slug: "configuration", icon: "settings" },
        { title: "Hardening", slug: "self-hosting-hardening", icon: "alert" },
        { title: "JS Rendering", slug: "js-rendering", icon: "zap" },
        { title: "TLS-fingerprint walls", slug: "tls-fingerprint-walls", icon: "shield" },
        { title: "Proxies", slug: "proxies", icon: "shield" },
      ],
    },
    {
      title: "Reference",
      children: [
        { title: "Response Shapes", slug: "response-shapes", icon: "layers" },
        { title: "Output Formats", slug: "output-formats", icon: "file-text" },
        { title: "Rate Limits", slug: "rate-limits", icon: "alert" },
        { title: "Error Codes", slug: "error-codes", icon: "info" },
        { title: "Credit Costs", slug: "credit-costs", icon: "list" },
        { title: "Compatibility", slug: "compatibility", icon: "check" },
        { title: "Firecrawl v2 Compatibility", slug: "v2-api", icon: "server" },
        { title: "Capabilities", slug: "capabilities", icon: "check" },
        { title: "Glossary", slug: "glossary", icon: "book" },
        { title: "Changelog", slug: "changelog", icon: "list" },
        { title: "Architecture", slug: "architecture", icon: "layers" },
        { title: "Crates", slug: "crates", icon: "box" },
      ],
    },
  ],

  defaultPage: "introduction",

  footer: {
    tagline: "The base layer for agentic web intelligence.",
    columns: [
      { title: "Product", links: [
        { label: "Quick Start", href: "/quick-start" },
        { label: "REST API", href: "/rest-api" },
        { label: "MCP Server", href: "/mcp" },
        { label: "Changelog", href: "/changelog" },
      ]},
      { title: "Community", links: [
        { label: "GitHub", href: "https://github.com/fastcrw/crw", external: true },
        { label: "Issues", href: "https://github.com/fastcrw/crw/issues", external: true },
        { label: "Discord", href: "https://discord.gg/KNQBfFVc9J", external: true },
      ]},
      { title: "Legal", links: [
        { label: "License (AGPL-3.0)", href: "https://github.com/fastcrw/crw/blob/main/LICENSE", external: true },
      ]},
    ],
    socials: [
      { icon: "github", href: "https://github.com/fastcrw/crw" },
      { icon: "discord", href: "https://discord.gg/KNQBfFVc9J" },
    ],
  },
};
