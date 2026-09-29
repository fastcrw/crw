/** CRW client — cloud (default), self-hosted HTTP, or local subprocess mode. */

import { CrwApiError, CrwError, CrwExtractCancelledError, CrwTimeoutError } from "./errors.js";
import { LocalTransport } from "./local.js";
import type {
  BatchResult,
  BatchScrapeOptions,
  Capabilities,
  ChangeTrackingOptions,
  ClientOptions,
  CrawlOptions,
  CrawlResult,
  DiffResult,
  ExtractOptions,
  ExtractAccepted,
  ExtractResult,
  ExtractStatus,
  Json,
  MapOptions,
  MapResult,
  ParseFileOptions,
  ParseResult,
  ScrapeOptions,
  ScrapeResult,
  SearchOptions,
  SearchResult,
  ResearchSearchOptions,
  ResearchReadOptions,
  ResearchSimilarOptions,
} from "./types.js";

// CRW is cloud-first: with no explicit apiUrl and no CRW_LOCAL opt-in, the client
// talks to the managed cloud. Mirrors the Python SDK + CLI onboarding.
export const CLOUD_API_URL = "https://api.fastcrw.com";
export const DASHBOARD_URL = "https://fastcrw.com/dashboard";
export const DOCS_URL = "https://docs.fastcrw.com";

const SIGNUP_NUDGE =
  `No CRW API key found. CRW uses the managed cloud (${CLOUD_API_URL}) by default.\n` +
  `  -> Sign up at ${DASHBOARD_URL} for 1000 free credits — no payment, no monthly ` +
  `reset (GitHub/Google, ~10s) — then set CRW_API_KEY (or pass apiKey).\n` +
  `  -> Prefer to self-host? Set CRW_LOCAL=1 to run the local engine. Docs: ${DOCS_URL}`;

function envTruthy(value: string | undefined): boolean {
  return !!value && !["0", "false", "no", ""].includes(value.trim().toLowerCase());
}

function httpOnlyHint(name: string, reason: string): string {
  return (
    `${name}() requires HTTP mode (${reason}). It is not available with CRW_LOCAL=1. ` +
    `Use the cloud (set CRW_API_KEY) or pass apiUrl for a self-hosted server.`
  );
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Pick `crw_map`'s payload out of whichever shape the local `crw-mcp` returned.
 * It answers flat (`{success, links, sitemaps}`) when it runs embedded and with
 * the REST envelope (`{success, data: {…}}`) when it proxies. It proxies
 * whenever `CRW_API_URL` is set, which the subprocess inherits from us even
 * though CRW_LOCAL mode never reads that variable itself. Without this, `links`
 * came back empty in that state.
 *
 * Probe `data.links` rather than a bare `data`, so a flat response that grows
 * some unrelated top-level `data` later cannot make us unwrap into it. Same
 * discriminator the engine's own MCP bounds use.
 */
function mapPayload(result: Json): Json {
  const envelope = result.data as Json | null | undefined;
  return envelope && typeof envelope === "object" && "links" in envelope ? envelope : result;
}

/**
 * Next page request, rebuilt from OUR base path + the cursor's skip/limit. The
 * server's `next` is an absolute URL from its own public origin/prefix; reusing
 * it verbatim double-prefixes when apiUrl carries a path (`.../api`) and trusts a
 * host that may not match apiUrl. We only borrow the cursor position.
 */
function pageCursorPath(basePath: string, nextUrl: string): string {
  let search = "";
  try {
    search = new URL(nextUrl, "http://placeholder.invalid").search;
  } catch {
    return basePath;
  }
  const q = new URLSearchParams(search);
  const params = ["skip", "limit"]
    .filter((k) => q.get(k) !== null)
    .map((k) => `${k}=${q.get(k)}`);
  return params.length ? `${basePath}?${params.join("&")}` : basePath;
}

export class CrwClient {
  private apiUrl: string | null;
  private apiKey: string | undefined;
  private local: LocalTransport | null = null;

  /**
   * CRW is cloud-first. With no arguments the client targets the managed cloud
   * (api.fastcrw.com) and needs an API key — sign up for 1000 free credits at
   * https://fastcrw.com/dashboard. To self-host locally, set `CRW_LOCAL=1`.
   */
  constructor(opts: ClientOptions = {}) {
    const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process
      ?.env ?? {};
    this.apiKey = opts.apiKey ?? env.CRW_API_KEY;

    if (envTruthy(env.CRW_LOCAL)) {
      // Self-host opt-in: zero-config local engine (subprocess), no key.
      this.apiUrl = null;
      return;
    }

    const explicitUrl = opts.apiUrl ?? env.CRW_API_URL;
    this.apiUrl = explicitUrl ?? CLOUD_API_URL;
    // Only the managed-cloud default requires a key; an explicit self-hosted
    // server may run without auth.
    if (explicitUrl === undefined && !this.apiKey) {
      throw new CrwError(SIGNUP_NUDGE);
    }
  }

  async scrape(url: string, opts: ScrapeOptions = {}): Promise<ScrapeResult> {
    const { formats, onlyMainContent = true, includeTags, excludeTags, renderJs, renderer, waitFor, jsonSchema, ...rest } = opts;
    const args: Json = { url, onlyMainContent };
    if (formats) args.formats = [...formats];
    if (includeTags) args.includeTags = includeTags;
    if (excludeTags) args.excludeTags = excludeTags;
    if (renderJs !== undefined) args.renderJs = renderJs;
    if (renderer !== undefined) args.renderer = renderer;
    if (waitFor !== undefined) args.waitFor = waitFor;
    if (jsonSchema !== undefined) {
      args.jsonSchema = jsonSchema;
      const f = (args.formats as string[] | undefined) ?? [];
      if (!f.includes("json")) args.formats = [...f, "json"];
    }
    Object.assign(args, rest);
    if (this.apiUrl) return this.httpPost("/v1/scrape", args) as Promise<ScrapeResult>;
    return this.localTransport().toolCall("crw_scrape", args) as Promise<ScrapeResult>;
  }

  async crawl(url: string, opts: CrawlOptions = {}): Promise<CrawlResult> {
    const { maxDepth = 2, maxPages = 10, pollInterval = 2, timeout = 300, ...rest } = opts;
    const args: Json = { url, maxDepth, maxPages, ...rest };
    if (this.apiUrl) return this.httpCrawl(args, pollInterval, timeout);
    const result = await this.localTransport().toolCall("crw_crawl", args);
    const jobId = result.id as string | undefined;
    if (!jobId) throw new CrwError(`Crawl did not return job ID: ${JSON.stringify(result)}`);
    return this.pollLocalCrawl(jobId, pollInterval, timeout);
  }

  async map(url: string, opts: MapOptions = {}): Promise<MapResult> {
    const { maxDepth = 2, useSitemap = true, ...rest } = opts;
    const args: Json = { url, maxDepth, useSitemap, ...rest };
    const data = this.apiUrl
      ? await this.httpPost("/v1/map", args)
      : mapPayload(await this.localTransport().toolCall("crw_map", args));
    // Guard the shapes, as search() does before its own attach: a self-hosted
    // server or gateway answering `{"links": "none"}` would otherwise make
    // Object.defineProperty throw, or smuggle a non-array out typed as string[].
    const links = (Array.isArray(data.links) ? (data.links as string[]) : []) as MapResult;
    // `sitemaps` rides BESIDE `links` in the engine's response and the old
    // unwrap dropped it: a caller could not see which sitemaps a site exposes
    // even though they paid for the discovery. Attach it the way search()
    // attaches its answer siblings: non-enumerably, so `links.sitemaps` works
    // while `for…of`, spread and JSON.stringify keep seeing exactly the array
    // they saw before.
    Object.defineProperty(links, "sitemaps", {
      value: Array.isArray(data.sitemaps) ? (data.sitemaps as string[]) : [],
      enumerable: false,
      configurable: true,
      writable: true,
    });
    return links;
  }

  /**
   * Works in both modes; local mode needs a search backend configured on the engine.
   */
  async search(query: string, opts: SearchOptions = {}): Promise<SearchResult> {
    const { limit = 5, lang, tbs, sources, categories, scrapeOptions, ...rest } = opts;
    const args: Json = { query, limit };
    if (lang) args.lang = lang;
    if (tbs) args.tbs = tbs;
    if (sources) args.sources = sources;
    if (categories) args.categories = categories;
    if (scrapeOptions) args.scrapeOptions = scrapeOptions;
    Object.assign(args, rest);
    if (this.apiUrl) {
      // `data` IS the results, in whatever shape the caller asked for: a flat
      // array, a `{ web, news }` object when `sources` is set, or the engine's
      // `{ results, answer, … }` when you point the client at a self-hosted
      // server. Return it untouched — reshaping it here silently emptied both the
      // grouped and the self-hosted cases.
      //
      // `answer` / `citations` / `llmUsage` ride BESIDE `data` on the managed API,
      // and the old `data` unwrap dropped them: you could pay for an AI answer and
      // have no way to read it. Attach them non-enumerably, so `results.answer`
      // works while `for…of`, spread and JSON.stringify keep seeing exactly the
      // array they saw before.
      const raw = await this.httpRequest("POST", "/v1/search", args, {
        raw: true,
      });
      const data = (raw.data ?? []) as Json;
      if (data !== null && typeof data === "object") {
        for (const key of ["answer", "citations", "llmUsage"] as const) {
          if (raw[key] !== undefined) {
            Object.defineProperty(data, key, {
              value: raw[key],
              enumerable: false,
              configurable: true,
              writable: true,
            });
          }
        }
      }
      return data as SearchResult;
    }
    return this.localTransport().toolCall("crw_search", args) as Promise<SearchResult>;
  }

  /**
   * Firecrawl-compatible Research API (cloud only). Mirrors the Firecrawl
   * research SDK surface over `/v1/search/research/*`. Each method GETs the
   * hosted endpoint and returns its `{ success, ... }` payload verbatim.
   */
  get research() {
    const get = (path: string, params: Record<string, unknown>) => {
      if (this.apiUrl === null)
        throw new CrwError("research API requires cloud mode (set apiKey/apiUrl)");
      const qs = Object.entries(params)
        .filter(([, v]) => v !== undefined && v !== null)
        .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
        .join("&");
      return this.httpRequest("GET", qs ? `${path}?${qs}` : path, undefined, {
        checkSuccess: false,
      });
    };
    return {
      searchPapers: (query: string, opts: ResearchSearchOptions = {}): Promise<Json> =>
        get("/v1/search/research/papers", { query, ...opts }),
      getPaper: (id: string, opts: ResearchReadOptions = {}): Promise<Json> =>
        get(`/v1/search/research/papers/${encodeURIComponent(id)}`, { ...opts }),
      similarPapers: (id: string, opts: ResearchSimilarOptions): Promise<Json> =>
        get(`/v1/search/research/papers/${encodeURIComponent(id)}/similar`, { ...opts }),
      searchGithub: (query: string, opts: { k?: number } = {}): Promise<Json> =>
        get("/v1/search/research/github", { query, ...opts }),
    };
  }

  /**
   * Parse a document (PDF) into markdown / structured JSON. Works in both modes.
   */
  async parseFile(content: Uint8Array, opts: ParseFileOptions = {}): Promise<ParseResult> {
    const { filename = "document.pdf", formats, jsonSchema, parsers, ...rest } = opts;
    if (this.apiUrl) {
      const options: Json = {};
      if (formats) options.formats = [...formats];
      if (jsonSchema !== undefined) options.jsonSchema = jsonSchema;
      if (parsers) options.parsers = parsers;
      Object.assign(options, rest);
      const form = new FormData();
      form.append("file", new Blob([content as unknown as BlobPart]), filename);
      if (Object.keys(options).length) form.append("options", JSON.stringify(options));
      return this.httpMultipart("/v2/parse", form) as Promise<ParseResult>;
    }
    const b64 = Buffer.from(content).toString("base64");
    const args: Json = { filename, contentBase64: b64 };
    if (formats) args.formats = [...formats];
    if (jsonSchema !== undefined) args.jsonSchema = jsonSchema;
    if (parsers) args.parsers = parsers;
    Object.assign(args, rest);
    return this.localTransport().toolCall("crw_parse_file", args) as Promise<ParseResult>;
  }

  /**
   * Start native multi-URL extraction without waiting. `Prefer: respond-async`
   * is always sent so managed and self-hosted servers use the same lifecycle.
   */
  async startExtract(opts: ExtractOptions): Promise<ExtractAccepted> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("startExtract", "LLM extract job endpoint"));
    const start = await this.startExtractRequest(opts);
    if (typeof start.id === "string" && start.status === "processing") {
      return start as unknown as ExtractAccepted;
    }
    throw new CrwError(`startExtract did not return an accepted job: ${JSON.stringify(start)}`);
  }

  /** Get the canonical lifecycle envelope for an extract job. */
  async getExtract(id: string): Promise<ExtractStatus> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("getExtract", "LLM extract job endpoint"));
    const result = await this.httpRequest("GET", `/v1/extract/${encodeURIComponent(id)}`, undefined, {
      raw: true,
      checkSuccess: false,
    });
    return result as unknown as ExtractStatus;
  }

  /** Idempotently request cancellation and return the canonical lifecycle envelope. */
  async cancelExtract(id: string): Promise<ExtractStatus> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("cancelExtract", "LLM extract job endpoint"));
    const result = await this.httpRequest("DELETE", `/v1/extract/${encodeURIComponent(id)}`, undefined, {
      raw: true,
      checkSuccess: false,
    });
    return result as unknown as ExtractStatus;
  }

  /**
   * Convenience waiter over start/get/cancel. `cancelling` is non-terminal;
   * terminal cancellation raises a typed error carrying partial results.
   */
  async extract(opts: ExtractOptions): Promise<ExtractResult> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("extract", "LLM extract job endpoint"));
    const { pollInterval = 2, timeout = 120 } = opts;
    const start = await this.startExtractRequest(opts);
    const jobId = start.id as string | undefined;
    // Preserve the managed synchronous response during rollout if an older
    // endpoint ignores Prefer. New managed/self-hosted fixtures return an id.
    if (!jobId) {
      if (start.status === "cancelled") {
        throw new CrwExtractCancelledError(start as unknown as ExtractStatus);
      }
      if (Array.isArray(start.results)) return start.results as ExtractResult;
      throw new CrwError(`extract returned neither a job id nor results: ${JSON.stringify(start)}`);
    }
    const deadline = Date.now() + timeout * 1000;
    for (;;) {
      if (Date.now() > deadline) {
        try {
          await this.cancelExtract(jobId);
        } catch {
          // Timeout remains the caller-visible failure; DELETE is best effort.
        }
        throw new CrwTimeoutError(`Extract ${jobId} timed out after ${timeout}s`);
      }
      const status = await this.getExtract(jobId);
      if (status.status === "completed") return status.results;
      if (status.status === "failed") throw new CrwError(`Extract failed: ${status.error ?? "unknown"}`);
      if (status.status === "cancelled") throw new CrwExtractCancelledError(status);
      await sleep(pollInterval * 1000);
    }
  }

  private async startExtractRequest(opts: ExtractOptions): Promise<Json> {
    const { urls, prompt, schema, basis, llmApiKey, llmProvider, llmModel, pollInterval = 2, timeout = 120 } = opts;
    void pollInterval;
    void timeout;
    const body: Json = { urls: [...urls] };
    if (prompt !== undefined) body.prompt = prompt;
    if (schema !== undefined) body.schema = schema;
    if (basis !== undefined) body.basis = basis;
    if (llmApiKey !== undefined) body.llmApiKey = llmApiKey;
    if (llmProvider !== undefined) body.llmProvider = llmProvider;
    if (llmModel !== undefined) body.llmModel = llmModel;
    return this.httpRequest("POST", "/v1/extract", body, {
      raw: true,
      headers: { Prefer: "respond-async" },
    });
  }

  /** Scrape many URLs in one async batch job (HTTP mode only). */
  async batchScrape(urls: string[], opts: BatchScrapeOptions = {}): Promise<BatchResult> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("batchScrape", "batch job endpoint"));
    const { formats, pollInterval = 2, timeout = 300, ...rest } = opts;
    const body: Json = { urls: [...urls], ...rest };
    if (formats) body.formats = [...formats];
    const start = await this.httpRequest("POST", "/v2/batch/scrape", body, { raw: true });
    const jobId = start.id as string | undefined;
    if (!jobId) throw new CrwError(`Batch scrape did not return job ID: ${JSON.stringify(start)}`);
    const deadline = Date.now() + timeout * 1000;
    const statusPath = `/v2/batch/scrape/${jobId}`;

    // Poll the first page until the job reaches a terminal state.
    let status: Json;
    for (;;) {
      if (Date.now() > deadline) throw new CrwTimeoutError(`Batch scrape ${jobId} timed out after ${timeout}s`);
      status = await this.httpRequest("GET", statusPath, undefined, { raw: true, checkSuccess: false });
      if (status.status === "completed") break;
      if (status.status === "failed") throw new CrwError(`Batch scrape failed: ${status.error ?? "unknown"}`);
      // A cancelled job never completes; stop instead of spinning until timeout.
      if (status.status === "cancelled") throw new CrwError(`Batch scrape ${jobId} was cancelled`);
      await sleep(pollInterval * 1000);
    }

    // Page through the full result set. The batch status returns at most ~100
    // documents per page and sets `next` to the cursor for the next page (null on
    // the last). Returning only page 1 silently truncated batches over one page.
    const docs: BatchResult = [...((status.data as BatchResult) ?? [])];
    let nextUrl = status.next as string | undefined;
    // Seed with page 1's path: a `next` that rebuilds to it (skip=0, or no cursor
    // at all) is a repeat, not a new page — stop rather than re-append.
    const seen = new Set<string>([statusPath]);
    while (nextUrl) {
      const pagePath = pageCursorPath(statusPath, nextUrl);
      if (seen.has(pagePath)) break; // repeating/empty cursor; nothing new
      if (Date.now() > deadline) throw new CrwTimeoutError(`Batch scrape ${jobId} timed out after ${timeout}s`);
      seen.add(pagePath);
      const page = await this.httpRequest("GET", pagePath, undefined, { raw: true, checkSuccess: false });
      docs.push(...((page.data as BatchResult) ?? []));
      nextUrl = page.next as string | undefined;
    }
    return docs;
  }

  /** Feature-detect the engine (HTTP mode only). */
  async capabilities(): Promise<Capabilities> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("capabilities", "server capabilities endpoint"));
    return this.httpRequest("GET", "/v1/capabilities", undefined, { checkSuccess: false }) as Promise<Capabilities>;
  }

  /** Diff a page against a prior snapshot (HTTP mode only). */
  async changeTrackingDiff(current: Json, previous?: Json, opts: ChangeTrackingOptions = {}): Promise<DiffResult> {
    if (!this.apiUrl) throw new CrwError(httpOnlyHint("changeTrackingDiff", "diff endpoint"));
    const { modes, schema, prompt, ...rest } = opts;
    const body: Json = { current, modes: modes ? [...modes] : ["gitDiff"] };
    if (previous !== undefined) body.previous = previous;
    if (schema !== undefined) body.schema = schema;
    if (prompt !== undefined) body.prompt = prompt;
    Object.assign(body, rest);
    return this.httpPost("/v1/change-tracking/diff", body) as Promise<DiffResult>;
  }

  /** Shut down the local subprocess if running. */
  close(): void {
    this.local?.close();
    this.local = null;
  }

  // --- local (subprocess) mode ---

  private localTransport(): LocalTransport {
    if (!this.local) this.local = new LocalTransport();
    return this.local;
  }

  private async pollLocalCrawl(jobId: string, pollInterval: number, timeout: number): Promise<CrawlResult> {
    const deadline = Date.now() + timeout * 1000;
    for (;;) {
      if (Date.now() > deadline) throw new CrwTimeoutError(`Crawl ${jobId} timed out after ${timeout}s`);
      const result = await this.localTransport().toolCall("crw_check_crawl_status", { id: jobId });
      if (result.status === "completed") return (result.data as CrawlResult) ?? [];
      if (result.status === "failed") throw new CrwError(`Crawl failed: ${result.error ?? "unknown"}`);
      if (result.status === "cancelled") throw new CrwError(`Crawl ${jobId} was cancelled`);
      await sleep(pollInterval * 1000);
    }
  }

  // --- HTTP mode ---

  private async httpRequest(
    method: string,
    path: string,
    body?: Json,
    {
      raw = false,
      checkSuccess = true,
      headers: extraHeaders,
    }: { raw?: boolean; checkSuccess?: boolean; headers?: Record<string, string> } = {},
  ): Promise<Json> {
    if (this.apiUrl === null) throw new CrwError("internal: httpRequest in local mode");
    const url = `${this.apiUrl.replace(/\/$/, "")}${path}`;
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...extraHeaders,
    };
    if (this.apiKey) headers.Authorization = `Bearer ${this.apiKey}`;
    const resp = await fetch(url, { method, headers, body: body ? JSON.stringify(body) : undefined });
    const result = await this.readJson(resp);
    if (checkSuccess && result.success === false) {
      throw new CrwApiError((result.error as string) ?? "API error", resp.status);
    }
    if (raw) return result;
    return (result.data as Json) ?? result;
  }

  private async httpMultipart(path: string, form: FormData): Promise<Json> {
    if (this.apiUrl === null) throw new CrwError("internal: httpMultipart in local mode");
    const url = `${this.apiUrl.replace(/\/$/, "")}${path}`;
    const headers: Record<string, string> = {};
    if (this.apiKey) headers.Authorization = `Bearer ${this.apiKey}`;
    const resp = await fetch(url, { method: "POST", headers, body: form });
    const result = await this.readJson(resp);
    if (result.success === false) throw new CrwApiError((result.error as string) ?? "API error", resp.status);
    return (result.data as Json) ?? result;
  }

  /** Parse the JSON body; surface a non-2xx body's `error` as CrwApiError. */
  private async readJson(resp: Response): Promise<Json> {
    const text = await resp.text();
    let parsed: Json;
    try {
      parsed = text ? (JSON.parse(text) as Json) : {};
    } catch {
      if (!resp.ok) throw new CrwApiError(`HTTP ${resp.status}: ${resp.statusText}`, resp.status);
      throw new CrwApiError(`Invalid JSON response (HTTP ${resp.status})`, resp.status);
    }
    if (!resp.ok) {
      const message = (parsed.error as string) ?? (parsed.message as string) ?? `HTTP ${resp.status}`;
      throw new CrwApiError(message, resp.status);
    }
    return parsed;
  }

  private httpPost(path: string, body: Json): Promise<Json> {
    return this.httpRequest("POST", path, body);
  }

  private async httpCrawl(args: Json, pollInterval: number, timeout: number): Promise<CrawlResult> {
    const result = await this.httpPost("/v1/crawl", args);
    const jobId = result.id as string | undefined;
    if (!jobId) throw new CrwError(`Crawl did not return job ID: ${JSON.stringify(result)}`);
    const deadline = Date.now() + timeout * 1000;
    for (;;) {
      if (Date.now() > deadline) throw new CrwTimeoutError(`Crawl ${jobId} timed out after ${timeout}s`);
      const status = await this.httpRequest("GET", `/v1/crawl/${jobId}`, undefined, { raw: true });
      if (status.status === "completed") return (status.data as CrawlResult) ?? [];
      if (status.status === "failed") throw new CrwError(`Crawl failed: ${status.error ?? "unknown"}`);
      if (status.status === "cancelled") throw new CrwError(`Crawl ${jobId} was cancelled`);
      await sleep(pollInterval * 1000);
    }
  }
}
