import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";
import {
  CLOUD_API_URL,
  CrwClient,
  CrwError,
  CrwExtractCancelledError,
  CrwTimeoutError,
} from "../dist/esm/index.js";
import { localChildEnv } from "../dist/esm/local.js";

const origFetch = globalThis.fetch;
const origEnv = { ...process.env };

beforeEach(() => {
  delete process.env.CRW_LOCAL;
  delete process.env.CRW_API_KEY;
  delete process.env.CRW_API_URL;
});

afterEach(() => {
  globalThis.fetch = origFetch;
  process.env = { ...origEnv };
});

function mockFetch(body: unknown, ok = true, status = 200) {
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    return {
      ok,
      status,
      statusText: "OK",
      text: async () => JSON.stringify(body),
    } as Response;
  }) as typeof fetch;
  return calls;
}

test("cloud-first: no key throws onboarding nudge", () => {
  assert.throws(() => new CrwClient(), /1000 free credits/);
});

test("cloud-first: with key targets the cloud", () => {
  const c = new CrwClient({ apiKey: "crw_live_test" });
  assert.equal((c as unknown as { apiUrl: string }).apiUrl, CLOUD_API_URL);
});

test("explicit apiUrl does not require a key", () => {
  const c = new CrwClient({ apiUrl: "http://localhost:3000" });
  assert.equal((c as unknown as { apiUrl: string }).apiUrl, "http://localhost:3000");
});

test("CRW_LOCAL=1 enables local mode (apiUrl null)", () => {
  process.env.CRW_LOCAL = "1";
  const c = new CrwClient();
  assert.equal((c as unknown as { apiUrl: string | null }).apiUrl, null);
});

test("scrape maps first-class params + jsonSchema adds json format", async () => {
  const calls = mockFetch({ success: true, data: { markdown: "x" } });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  await c.scrape("https://example.com", { formats: ["markdown"], renderJs: true, waitFor: 1500, jsonSchema: { type: "object" } });
  const body = JSON.parse((calls[0].init!.body as string));
  assert.equal(calls[0].url, `${CLOUD_API_URL}/v1/scrape`);
  assert.equal(body.renderJs, true);
  assert.equal(body.waitFor, 1500);
  assert.deepEqual(body.jsonSchema, { type: "object" });
  assert.ok(body.formats.includes("json") && body.formats.includes("markdown"));
  assert.equal("onlyMainContent" in body, false, "omitted: server default applies");
});

test("scrape sends onlyMainContent only when set", async () => {
  const calls = mockFetch({ success: true, data: { markdown: "x" } });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  await c.scrape("https://example.com", { onlyMainContent: false });
  assert.equal(JSON.parse(calls[0].init!.body as string).onlyMainContent, false);
});

test("scrape result exposes typed document fields", async () => {
  mockFetch({
    success: true,
    data: {
      markdown: "# Hi",
      links: ["https://example.com/a"],
      images: [{ url: "https://example.com/i.png", alt: "i" }],
      metadata: { title: "Ex", sourceURL: "https://example.com", statusCode: 200, elapsedMs: 12 },
    },
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const result = await c.scrape("https://example.com");
  // These property accesses only compile because ScrapeResult is now typed.
  const md: string | undefined = result.markdown;
  const src: string = result.metadata.sourceURL;
  const code: number = result.metadata.statusCode;
  const firstLink: string | undefined = result.links?.[0];
  const alt: string | undefined = result.images?.[0].alt;
  assert.equal(md, "# Hi");
  assert.equal(src, "https://example.com");
  assert.equal(code, 200);
  assert.equal(firstLink, "https://example.com/a");
  assert.equal(alt, "i");
});

test("search result carries typed items and answer siblings", async () => {
  mockFetch({
    success: true,
    data: [{ url: "https://example.com", title: "Ex", description: "d", position: 1 }],
    answer: "42",
    citations: [{ url: "https://example.com", title: "Ex", position: 1 }],
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const results = await c.search("q");
  assert.ok(Array.isArray(results));
  if (Array.isArray(results)) {
    const title: string = results[0].title;
    assert.equal(title, "Ex");
  }
  // `answer` rides alongside the array (attached non-enumerably by the client).
  assert.equal((results as { answer?: string }).answer, "42");
});

test("capabilities result is a typed feature payload", async () => {
  mockFetch({
    version: "0.27.1",
    llm: { providers: ["managed"], supportsBaseUrl: false, serverKeyConfigured: true, maxConcurrency: 4 },
    formats: { supported: ["markdown"], llmRequired: [], changeTrackingModes: [], changeTrackingModesLlmRequired: [] },
    search: { supported: true, answer: true, summarizeResults: true },
    screenshot: { supported: false, fullPage: false },
    renderers: { available: ["chrome"], mode: "auto" },
    extract: { supported: true, maxUrls: 10, perFieldAttribution: true, maxOutputTokens: 4096 },
    documents: { parsers: ["pdf"], fileUpload: { supported: true, endpoint: "/v2/parse", maxBytes: 1, types: ["pdf"], ocr: false } },
    limits: { maxBatchUrls: 1, maxExtractUrls: 1, searchDefaultLimit: 5, searchMaxLimit: 20, maxUploadBytes: 1 },
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const caps = await c.capabilities();
  const version: string = caps.version;
  const providers: string[] = caps.llm.providers;
  assert.equal(version, "0.27.1");
  assert.deepEqual(providers, ["managed"]);
});

test("HTTP-only methods throw in local mode", async () => {
  process.env.CRW_LOCAL = "1";
  const c = new CrwClient();
  await assert.rejects(() => c.extract({ urls: ["https://example.com"] }), /requires HTTP mode/);
  await assert.rejects(() => c.startExtract({ urls: ["https://example.com"] }), /requires HTTP mode/);
  await assert.rejects(() => c.getExtract("job-1"), /requires HTTP mode/);
  await assert.rejects(() => c.cancelExtract("job-1"), /requires HTTP mode/);
  await assert.rejects(() => c.batchScrape(["https://example.com"]), /requires HTTP mode/);
  await assert.rejects(() => c.capabilities(), /requires HTTP mode/);
  await assert.rejects(() => c.changeTrackingDiff({ markdown: "a" }), /requires HTTP mode/);
});

test("non-2xx body surfaces engine error as CrwApiError", async () => {
  mockFetch({ error: "boom" }, false, 400);
  const c = new CrwClient({ apiKey: "crw_live_test" });
  await assert.rejects(() => c.scrape("https://example.com"), /boom/);
});

test("batchScrape follows the next cursor to return every page", async () => {
  const responses: unknown[] = [
    { success: true, id: "b1" }, // start
    {
      success: true,
      status: "completed",
      data: [{ markdown: "p1" }],
      next: "https://fastcrw.com/api/v2/batch/scrape/b1?skip=1",
    },
    { success: true, status: "completed", data: [{ markdown: "p2" }], next: null },
  ];
  const calls: string[] = [];
  globalThis.fetch = (async (url: string) => {
    calls.push(String(url));
    const body = responses.shift();
    return { ok: true, status: 200, statusText: "OK", text: async () => JSON.stringify(body) } as Response;
  }) as typeof fetch;
  const c = new CrwClient({ apiUrl: "https://fastcrw.com/api" });
  const docs = await c.batchScrape(["a", "b"]);
  assert.deepEqual(docs, [{ markdown: "p1" }, { markdown: "p2" }]);
  // Page 2 rebuilt from our base path + the cursor, no double `/api` prefix.
  assert.equal(calls[2], "https://fastcrw.com/api/v2/batch/scrape/b1?skip=1");
});

test("batchScrape stops on a cancelled job instead of hanging", async () => {
  const responses: unknown[] = [
    { success: true, id: "b1" },
    { success: true, status: "cancelled" },
  ];
  let n = 0;
  globalThis.fetch = (async () => {
    n++;
    const body = responses.shift();
    return { ok: true, status: 200, statusText: "OK", text: async () => JSON.stringify(body) } as Response;
  }) as typeof fetch;
  const c = new CrwClient({ apiUrl: "https://fastcrw.com/api" });
  await assert.rejects(() => c.batchScrape(["a"]), /cancelled/);
  assert.equal(n, 2); // start + one poll, no spinning
});

test("extract starts a /v1/extract job and returns per-URL results", async () => {
  let n = 0;
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    const body =
      n++ === 0
        ? { success: true, id: "job-1" }
        : {
            success: true,
            status: "completed",
            results: [{ url: "https://example.com", status: "completed", data: { title: "Hi" } }],
          };
    return { ok: true, status: 200, statusText: "OK", text: async () => JSON.stringify(body) } as Response;
  }) as typeof fetch;

  const c = new CrwClient({ apiKey: "crw_live_test" });
  const results = await c.extract({ urls: ["https://example.com"], schema: { type: "object" }, llmApiKey: "sk" });

  assert.equal(calls[0].url, `${CLOUD_API_URL}/v1/extract`);
  const postBody = JSON.parse(calls[0].init!.body as string);
  assert.deepEqual(postBody.urls, ["https://example.com"]);
  assert.equal(postBody.llmApiKey, "sk");
  assert.equal((calls[0].init!.headers as Record<string, string>).Prefer, "respond-async");
  assert.ok(calls[1].url.startsWith(`${CLOUD_API_URL}/v1/extract/job-1`));
  assert.equal(results.length, 1);
  const r0 = results[0] as { url: string; data: { title: string } };
  assert.equal(r0.url, "https://example.com");
  assert.equal(r0.data.title, "Hi");
});

test("startExtract sends Prefer for managed and self-hosted fixtures", async () => {
  const calls = mockFetch({ id: "job-1", status: "processing", urls: 1 });
  const managed = new CrwClient({ apiKey: "crw_live_test" });
  assert.equal((await managed.startExtract({ urls: ["https://example.com"], prompt: "x" })).id, "job-1");
  const selfHosted = new CrwClient({ apiUrl: "http://localhost:3000" });
  await selfHosted.startExtract({ urls: ["https://example.com"], schema: { type: "object" } });

  assert.equal(calls[0].url, `${CLOUD_API_URL}/v1/extract`);
  assert.equal(calls[1].url, "http://localhost:3000/v1/extract");
  for (const call of calls) {
    assert.equal((call.init!.headers as Record<string, string>).Prefer, "respond-async");
  }
});

test("extract preserves managed synchronous fixture while requesting async", async () => {
  const calls = mockFetch({
    success: true,
    results: [{ url: "https://example.com", status: "completed", data: { title: "Hi" } }],
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const results = await c.extract({ urls: ["https://example.com"], prompt: "title" });
  assert.equal(results[0].status, "completed");
  assert.equal((calls[0].init!.headers as Record<string, string>).Prefer, "respond-async");
});

test("getExtract and cancelExtract use canonical status route", async () => {
  const status = {
    id: "job/one",
    status: "cancelled",
    results: [{ url: "https://example.com", status: "cancelled" }],
    expiresAt: "2026-07-14T00:00:00Z",
    creditsUsed: 0,
    tokensUsed: 0,
  };
  const calls = mockFetch(status);
  const c = new CrwClient({ apiUrl: "http://localhost:3000" });
  assert.equal((await c.getExtract("job/one")).status, "cancelled");
  assert.equal((await c.cancelExtract("job/one")).results.length, 1);
  assert.equal(calls[0].url, "http://localhost:3000/v1/extract/job%2Fone");
  assert.equal(calls[0].init!.method, "GET");
  assert.equal(calls[1].init!.method, "DELETE");
});

test("extract treats cancelling as non-terminal then raises typed cancelled error", async () => {
  const responses = [
    { id: "job-1", status: "processing", urls: 2 },
    {
      id: "job-1",
      status: "cancelling",
      results: [
        { url: "https://a.example", status: "completed", data: { title: "A" } },
        { url: "https://b.example", status: "processing" },
      ],
      expiresAt: "2026-07-14T00:00:00Z",
      creditsUsed: 1,
      tokensUsed: 9,
    },
    {
      id: "job-1",
      status: "cancelled",
      results: [
        { url: "https://a.example", status: "completed", data: { title: "A" } },
        { url: "https://b.example", status: "cancelled" },
      ],
      expiresAt: "2026-07-14T00:00:00Z",
      creditsUsed: 1,
      tokensUsed: 9,
    },
  ];
  globalThis.fetch = (async () => {
    const body = responses.shift();
    return { ok: true, status: 200, statusText: "OK", text: async () => JSON.stringify(body) } as Response;
  }) as typeof fetch;
  const c = new CrwClient({ apiUrl: "http://localhost:3000" });
  await assert.rejects(
    () => c.extract({ urls: ["https://a.example", "https://b.example"], prompt: "x", pollInterval: 0 }),
    (error: unknown) => {
      assert.ok(error instanceof CrwExtractCancelledError);
      assert.equal(error.status.status, "cancelled");
      assert.equal(error.results[0].status, "completed");
      return true;
    },
  );
});

test("extract timeout performs best-effort DELETE", async () => {
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    const body = init?.method === "POST"
      ? { id: "job-1", status: "processing", urls: 1 }
      : {
          id: "job-1",
          status: "cancelled",
          results: [{ url: "https://example.com", status: "cancelled" }],
          expiresAt: "2026-07-14T00:00:00Z",
          creditsUsed: 0,
          tokensUsed: 0,
        };
    return { ok: true, status: 200, statusText: "OK", text: async () => JSON.stringify(body) } as Response;
  }) as typeof fetch;
  const c = new CrwClient({ apiUrl: "http://localhost:3000" });
  await assert.rejects(
    () => c.extract({ urls: ["https://example.com"], prompt: "x", timeout: -1 }),
    CrwTimeoutError,
  );
  assert.equal(calls.at(-1)?.init?.method, "DELETE");
});

test("capabilities unwraps and uses GET /v1/capabilities", async () => {
  const calls = mockFetch({ version: "0.14.0" });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const caps = await c.capabilities();
  assert.equal(calls[0].url, `${CLOUD_API_URL}/v1/capabilities`);
  assert.equal((caps as { version: string }).version, "0.14.0");
});

test("map returns the links array with sitemaps attached", async () => {
  const calls = mockFetch({
    success: true,
    data: {
      links: ["https://example.com/a", "https://example.com/b"],
      sitemaps: ["https://example.com/sitemap.xml"],
    },
    creditCost: 1,
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const links = await c.map("https://example.com");
  assert.equal(calls[0].url, `${CLOUD_API_URL}/v1/map`);
  assert.deepEqual([...links], ["https://example.com/a", "https://example.com/b"]);
  // The whole point of #557: the sitemaps the engine reported are reachable.
  const sitemaps: string[] = links.sitemaps ?? [];
  assert.deepEqual(sitemaps, ["https://example.com/sitemap.xml"]);
});

test("map normalizes a missing sitemaps field to an empty list", async () => {
  // An engine old enough not to send the key must not surface `undefined`,
  // so `links.sitemaps.length` is always safe.
  mockFetch({ success: true, data: { links: ["https://example.com/a"] } });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const links = await c.map("https://example.com");
  assert.deepEqual(links.sitemaps, []);
  assert.equal(links.sitemaps.length, 0);
});

test("map result is still a plain array for existing callers", async () => {
  mockFetch({
    success: true,
    data: { links: ["https://example.com/a"], sitemaps: ["https://example.com/sitemap.xml"] },
  });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const links = await c.map("https://example.com");
  assert.ok(Array.isArray(links));
  assert.equal(links.length, 1);
  assert.equal([...links].length, 1);
  // `sitemaps` is non-enumerable, so nothing an existing caller does changes.
  assert.equal(JSON.stringify(links), JSON.stringify(["https://example.com/a"]));
  assert.deepEqual(Object.keys(links), ["0"]);
});

/**
 * Drive local (subprocess) mode without a real crw-mcp: stand a stub in for the
 * transport the client would otherwise spawn. Mirrors the Python suite's
 * `patch.object(client, "_tool_call", ...)`.
 */
function localClientReturning(toolResult: unknown) {
  process.env.CRW_LOCAL = "1";
  const c = new CrwClient();
  (c as unknown as { local: { toolCall: () => Promise<unknown>; close: () => void } }).local = {
    toolCall: async () => toolResult,
    close: () => {},
  };
  return c;
}

test("map survives a malformed links/sitemaps body", async () => {
  // A self-hosted server or gateway can answer anything; neither field may
  // throw out of the SDK or escape typed as string[] when it is not one.
  mockFetch({ success: true, data: { links: "none", sitemaps: 3 } });
  const c = new CrwClient({ apiKey: "crw_live_test" });
  const links = await c.map("https://example.com");
  assert.ok(Array.isArray(links));
  assert.deepEqual([...links], []);
  assert.deepEqual(links.sitemaps, []);
});

test("map in local mode reads the flat embedded shape", async () => {
  const c = localClientReturning({
    success: true,
    links: ["https://example.com/a"],
    sitemaps: ["https://example.com/sitemap.xml"],
  });
  const links = await c.map("https://example.com");
  assert.deepEqual([...links], ["https://example.com/a"]);
  assert.deepEqual(links.sitemaps, ["https://example.com/sitemap.xml"]);
});

test("map in local mode unwraps the proxy REST envelope", async () => {
  // A crw-mcp that inherited CRW_API_URL proxies and answers with the envelope.
  // Without the unwrap this returned [] and dropped sitemaps entirely.
  const c = localClientReturning({
    success: true,
    data: {
      links: ["https://example.com/a"],
      sitemaps: ["https://example.com/sitemap.xml"],
    },
    creditCost: 1,
  });
  const links = await c.map("https://example.com");
  assert.deepEqual([...links], ["https://example.com/a"]);
  assert.deepEqual(links.sitemaps, ["https://example.com/sitemap.xml"]);
});

test("map in local mode ignores an unrelated top-level data field", async () => {
  // The probe is `data.links`, not a bare `data`: a flat response that grows
  // some other top-level `data` must not be unwrapped into.
  const c = localClientReturning({
    success: true,
    links: ["https://example.com/a"],
    sitemaps: ["https://example.com/sitemap.xml"],
    data: { note: 1 },
  });
  const links = await c.map("https://example.com");
  assert.deepEqual([...links], ["https://example.com/a"]);
  assert.deepEqual(links.sitemaps, ["https://example.com/sitemap.xml"]);
});

test("map in local mode does not unwrap a null data field", async () => {
  const c = localClientReturning({ success: true, data: null });
  const links = await c.map("https://example.com");
  assert.deepEqual([...links], []);
  assert.deepEqual(links.sitemaps, []);
});

test("the local subprocess does not inherit CRW_API_URL", () => {
  // crw-mcp binds CRW_API_URL itself and proxies to the cloud when it is set,
  // so an inherited one turned CRW_LOCAL into a billed cloud call answering
  // with the REST envelope instead of the flat payload.
  process.env.CRW_API_URL = "https://api.fastcrw.com";
  process.env.CRW_CLIENT__API_URL = "https://api.fastcrw.com";
  process.env.CRW_API_KEY = "crw_live_test";
  const env = localChildEnv();
  assert.equal(env.CRW_API_URL, undefined);
  assert.equal(env.CRW_CLIENT__API_URL, undefined);
  // The key is harmless: an embedded child ignores it.
  assert.equal(env.CRW_API_KEY, "crw_live_test");
});

// silence unused import lint in some configs
void CrwError;
