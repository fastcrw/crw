/**
 * Request/response types for the CRW SDK.
 *
 * Hand-written against the engine's OpenAPI spec
 * (crates/crw-server/openapi/openapi.json). Results are returned as the engine's
 * raw JSON objects, so the result aliases are intentionally permissive.
 */

export type Json = Record<string, unknown>;

export interface ClientOptions {
  /** Explicit server URL (self-hosted). Defaults to the managed cloud. */
  apiUrl?: string;
  /** API key for the cloud or an authenticated self-hosted server. */
  apiKey?: string;
}

export interface ScrapeOptions {
  formats?: string[];
  onlyMainContent?: boolean;
  includeTags?: string[];
  excludeTags?: string[];
  /** Force the JS renderer on/off (engine `renderJs`). */
  renderJs?: boolean;
  /** Pin a renderer tier (engine `renderer`). */
  renderer?: string;
  /** Milliseconds to wait after load before extracting (`waitFor`). */
  waitFor?: number;
  /** JSON Schema for structured LLM extraction (auto-adds the `json` format). */
  jsonSchema?: Json;
  /**
   * Return per-field evidence alongside `json`. Every top-level scalar property
   * of `jsonSchema` comes back as a {@link Basis}. Requires `jsonSchema`.
   */
  basis?: boolean;
  /** Any other engine scrape option, passed through verbatim. */
  [key: string]: unknown;
}

/** How well an extracted field is attributed to its source. */
export type FieldStatus = "supported" | "unverified" | "unsupported" | "notFound";

/** The citation backing an extracted value. Every field is server-produced. */
export interface EvidenceCitation {
  /** The document the engine fetched. Never a model-supplied url. */
  url: string;
  title?: string;
  /**
   * Verbatim span from the canonical source text. Absent when the span could
   * not be established (`status: "unverified"`).
   */
  excerpt?: string;
  /** `sha256:<hex>` of the exact text sent to the model. */
  sourceHash: string;
  /** Which text `sourceHash` covers. `"llmInput"` for extraction basis. */
  sourceTextKind: string;
}

/**
 * Per-field extraction evidence. Emitted only for top-level scalar schema
 * properties, and only when the request set `basis: true`.
 *
 * `status` is honest: a field whose attribution could not be verified is marked
 * `unverified`/`unsupported` rather than given a fabricated citation.
 * `citations` is empty exactly when `status` is `unsupported` or `notFound`;
 * `value` is `null` exactly when `status` is `notFound`.
 */
export interface Basis {
  basisVersion: number;
  field: string;
  value: unknown | null;
  status: FieldStatus;
  confidence?: "low" | "medium" | "high";
  citations: EvidenceCitation[];
}

/** A coded reason a field's attribution was downgraded. Never upstream text. */
export interface BasisWarning {
  field: string;
  code: string;
}

export interface CrawlOptions {
  maxDepth?: number;
  maxPages?: number;
  pollInterval?: number;
  timeout?: number;
  [key: string]: unknown;
}

export interface MapOptions {
  maxDepth?: number;
  useSitemap?: boolean;
  [key: string]: unknown;
}

export interface SearchOptions {
  limit?: number;
  lang?: string;
  tbs?: string;
  sources?: string[];
  categories?: string[];
  scrapeOptions?: Json;
  [key: string]: unknown;
}

export interface ParseFileOptions {
  filename?: string;
  formats?: string[];
  jsonSchema?: Json;
  parsers?: string[];
  [key: string]: unknown;
}

export interface ExtractOptions {
  urls: string[];
  prompt?: string;
  schema?: Json;
  /**
   * Return per-field evidence: each result carries a `basis` array, one
   * {@link Basis} per top-level scalar schema property. Requires `schema`.
   */
  basis?: boolean;
  /** BYOK: use your own LLM key/provider/model instead of the server's. */
  llmApiKey?: string;
  llmProvider?: string;
  llmModel?: string;
  pollInterval?: number;
  timeout?: number;
}

export type ExtractJobState =
  | "processing"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled";

export type ExtractUrlState = "processing" | "completed" | "failed" | "cancelled";

/** Response from an async extract admission request. */
export interface ExtractAccepted {
  id: string;
  status: "processing";
  /** Count of URLs enqueued for fetch. */
  urls: number;
}

/** One fixed, ordered result slot for a requested URL. */
export interface ExtractUrlResult {
  url: string;
  status: ExtractUrlState;
  data?: unknown;
  error?: string;
  llmUsage?: Json;
  basis?: Basis[];
  basisWarnings?: BasisWarning[];
  llmInputHash?: string;
}

/** Canonical GET/DELETE extract lifecycle envelope. */
export interface ExtractStatus {
  id: string;
  status: ExtractJobState;
  results: ExtractUrlResult[];
  error?: string;
  expiresAt: string;
  creditsUsed: number;
  tokensUsed: number;
}

export interface BatchScrapeOptions {
  formats?: string[];
  pollInterval?: number;
  timeout?: number;
  [key: string]: unknown;
}

export interface ChangeTrackingOptions {
  modes?: string[];
  schema?: Json;
  prompt?: string;
  [key: string]: unknown;
}

/** Token usage for an LLM-backed call (extract, summary, search answer). */
export interface LlmUsage {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  estimatedCostUsd?: number;
  model: string;
  provider: string;
  cacheHitInputTokens?: number;
  cacheMissInputTokens?: number;
  truncated?: boolean;
  calls?: number;
  executedSummaries: number;
  answerExecuted: boolean;
  [key: string]: unknown;
}

/** A scraped image reference. */
export interface ScrapedImage {
  url: string;
  alt?: string;
}

/** One semantic chunk, present when the request set a `chunkStrategy`. */
export interface Chunk {
  content: string;
  index: number;
  score?: number;
}

/** Anti-bot verdict, present only when the page was a block/challenge. */
export interface BlockOutcome {
  vendor: string;
  reason: string;
}

/** Which renderer tier produced the page, and why (`kind` is the discriminant). */
export interface RenderDecision {
  kind: string;
  [key: string]: unknown;
}

/**
 * Page metadata. `title`/`description` are always present as keys (may be
 * `null`); every other field appears only when the engine could resolve it.
 * Extra `<meta>` tags are flattened onto the object, so unknown keys can appear.
 */
export interface ScrapeMetadata {
  /** Always present on a `/v1` scrape (may be `null`); absent on some parse results. */
  title?: string | null;
  description?: string | null;
  ogTitle?: string;
  ogDescription?: string;
  ogImage?: string;
  canonicalUrl?: string;
  /** Canonical source URL (note the capitalized `URL`). */
  sourceURL: string;
  language?: string;
  statusCode: number;
  renderedWith?: string;
  elapsedMs: number;
  /** Page count for multi-page documents (e.g. PDFs). */
  numPages?: number;
  sourceFilename?: string;
  [key: string]: unknown;
}

/**
 * A scraped document. Which fields are present is gated by the request
 * `formats`; `metadata` is always returned. Unknown keys are preserved so the
 * type stays forward-compatible with newer engine fields.
 */
export interface ScrapeDocument {
  markdown?: string;
  html?: string;
  rawHtml?: string;
  /** Present when `formats` includes `plainText`. */
  plainText?: string;
  links?: string[];
  /** Present when `formats` includes `images`. */
  images?: ScrapedImage[];
  /** Structured extraction, present when a `jsonSchema` was supplied. */
  json?: Json;
  /** Present when `formats` includes `summary`. */
  summary?: string;
  /** A `data:image/png;base64,…` URL; present when `formats` includes `screenshot`. */
  screenshot?: string;
  metadata: ScrapeMetadata;
  contentType?: string;
  /** `sha256:`-prefixed hash of the canonical markdown. */
  sourceHash?: string;
  /** Per-field extraction evidence, present when the request set `basis: true`. */
  basis?: Basis[];
  basisWarnings?: BasisWarning[];
  llmInputHash?: string;
  llmUsage?: LlmUsage;
  chunks?: Chunk[];
  warning?: string;
  warnings?: string[];
  renderDecision?: RenderDecision;
  creditCost?: number;
  /** Present when `formats` includes `changeTracking`. */
  changeTracking?: ChangeTrackingResult;
  /** Present only when the page was a block/challenge. */
  block?: BlockOutcome;
  truncated?: boolean;
  [key: string]: unknown;
}

/** One search hit. Scrape fields appear only when `scrapeOptions` requested them. */
export interface SearchResultItem {
  url: string;
  title: string;
  description: string;
  /** Alias of `description` (Firecrawl parity). */
  snippet: string;
  position: number;
  score?: number;
  publishedDate?: string;
  category?: string;
  markdown?: string;
  html?: string;
  rawHtml?: string;
  links?: string[];
  metadata?: ScrapeMetadata;
  summary?: string;
  /** Enrichment-scrape error for this hit, when `scrapeOptions` failed. */
  error?: string;
  truncated?: boolean;
  [key: string]: unknown;
}

/** One image-search hit. */
export interface ImageResultItem {
  url: string;
  title: string;
  description: string;
  imageUrl: string;
  position: number;
  thumbnailUrl?: string;
  imageFormat?: string;
  resolution?: string;
  [key: string]: unknown;
}

/** A source citation backing a synthesized search `answer`. */
export interface SearchCitation {
  url: string;
  title: string;
  position: number;
}

/** Results grouped by source, returned when the request set `sources`. */
export interface GroupedSearchResults {
  web?: SearchResultItem[];
  news?: SearchResultItem[];
  images?: ImageResultItem[];
  [key: string]: unknown;
}

/**
 * Answer-mode siblings. On the managed API these ride alongside the results
 * (attached to the returned array/object); `answer` is present only when the
 * request enabled answer synthesis. Note: on the flat-array/grouped shapes the
 * client attaches them non-enumerably, so `JSON.stringify(result)` omits them —
 * read them off the result directly.
 */
export interface SearchAnswer {
  answer?: string;
  citations?: SearchCitation[];
  llmUsage?: LlmUsage;
  warnings?: string[];
}

/**
 * Self-hosted search envelope: results plus answer-mode siblings in one object.
 * A self-hosted server (custom `apiUrl`) returns this shape rather than the
 * managed API's flat/grouped {@link SearchResult}; cast to it with `as` when you
 * point the client at your own engine.
 */
export interface SearchResponseData extends SearchAnswer {
  results: SearchResultItem[] | GroupedSearchResults;
  [key: string]: unknown;
}

export type ChangeStatus = "same" | "changed";

export interface ChangeTrackingSnapshot {
  markdown?: string;
  json?: Json;
  contentHash: string;
  capturedAt?: string;
}

export interface ChangeDiff {
  /** Unified diff text (gitDiff mode). */
  text?: string;
  /** Structured diff: a parse-diff AST, or a per-field `{ previous, current }` map. */
  json?: Json;
}

export interface MeaningfulChange {
  type: string;
  before?: string;
  after?: string;
  reason: string;
}

export interface ChangeJudgment {
  meaningful: boolean;
  confidence: "low" | "medium" | "high";
  reason: string;
  meaningfulChanges: MeaningfulChange[];
}

export interface ChangeTrackingResult {
  status: ChangeStatus;
  firstObservation: boolean;
  contentHash: string;
  snapshot?: ChangeTrackingSnapshot;
  diff?: ChangeDiff;
  judgment?: ChangeJudgment;
  tag?: string;
  truncated?: boolean;
  [key: string]: unknown;
}

export interface LlmCapabilities {
  providers: string[];
  supportsBaseUrl: boolean;
  serverKeyConfigured: boolean;
  maxConcurrency: number;
  requireByokHeader?: string;
}

export interface FormatCapabilities {
  supported: string[];
  llmRequired: string[];
  changeTrackingModes: string[];
  changeTrackingModesLlmRequired: string[];
}

export interface SearchCapabilities {
  supported: boolean;
  answer: boolean;
  summarizeResults: boolean;
}

export interface ScreenshotCapabilities {
  supported: boolean;
  fullPage: boolean;
}

export interface RendererCapabilities {
  available: string[];
  mode: string;
  renderJsDefault?: boolean;
}

export interface ExtractCapabilities {
  supported: boolean;
  maxUrls: number;
  perFieldAttribution: boolean;
  maxOutputTokens: number;
}

export interface FileUploadCapabilities {
  supported: boolean;
  endpoint: string;
  maxBytes: number;
  types: string[];
  ocr: boolean;
}

export interface DocumentCapabilities {
  parsers: string[];
  fileUpload: FileUploadCapabilities;
}

export interface Limits {
  maxBatchUrls: number;
  maxExtractUrls: number;
  searchDefaultLimit: number;
  searchMaxLimit: number;
  maxUploadBytes: number;
}

/** Engine feature-detection payload (`GET /v1/capabilities`). */
export interface Capabilities {
  version: string;
  llm: LlmCapabilities;
  formats: FormatCapabilities;
  search: SearchCapabilities;
  screenshot: ScreenshotCapabilities;
  renderers: RendererCapabilities;
  extract: ExtractCapabilities;
  documents: DocumentCapabilities;
  limits: Limits;
  [key: string]: unknown;
}

/**
 * Page metadata on the Firecrawl-compatible `/v2` surface (parse, batch scrape).
 * Differs from {@link ScrapeMetadata}: no `elapsedMs`/`og*`, and it carries
 * v2-synthesized fields (`url`, `proxyUsed`, `cacheState`, `creditsUsed`, …).
 */
export interface V2Metadata {
  title?: string;
  description?: string;
  language?: string;
  /** Canonical source URL (note the capitalized `URL`). */
  sourceURL: string;
  url: string;
  statusCode: number;
  contentType?: string;
  proxyUsed: string;
  cacheState: string;
  concurrencyLimited: boolean;
  creditsUsed: number;
  scrapeId: string;
  /** Page count for paginated documents (PDFs). */
  numPages?: number;
  sourceFilename?: string;
  [key: string]: unknown;
}

/**
 * A document from the Firecrawl-compatible `/v2` surface (parse, batch scrape).
 * Unlike {@link ScrapeDocument}, `images` is a flat array of URL strings and the
 * metadata is a {@link V2Metadata}.
 */
export interface V2Document {
  markdown?: string;
  html?: string;
  rawHtml?: string;
  links?: string[];
  /** Flat array of image URLs (v2 flattens the native `{ url, alt }` objects). */
  images?: string[];
  json?: Json;
  summary?: string;
  changeTracking?: ChangeTrackingResult;
  screenshot?: string;
  warning?: string;
  metadata: V2Metadata;
  [key: string]: unknown;
}

/** A scraped document (`/v1/scrape`). */
export type ScrapeResult = ScrapeDocument;
/** Crawl (`/v1/crawl`) returns the scraped document for each page, in crawl order. */
export type CrawlResult = ScrapeDocument[];
/**
 * Map (`/v1/map`) returns the discovered URLs, with the sitemap URLs the engine
 * read attached as `sitemaps`, the same way {@link SearchAnswer} rides
 * alongside search results. Assignable to `string[]`, so
 * `const urls: string[] = await crw.map(u)` keeps compiling.
 *
 * `sitemaps` is always an array at runtime: the client normalizes a missing
 * field to `[]`. It is typed optional only so a plain `string[]` still assigns
 * to this type, which keeps a consumer's own test stub
 * (`mockResolvedValue(["https://a"])`) compiling. Same shape as
 * {@link SearchAnswer}. It is non-enumerable, so it does not survive
 * `structuredClone` (and therefore an RSC, worker or `postMessage` boundary) or
 * any transform that builds a new array (`slice`, `filter`, spread).
 * `JSON.stringify` is unaffected, since an array serializes its indices either
 * way. Read it off the returned array directly.
 *
 * In local (subprocess) mode the MCP layer caps both lists at its map limit, so
 * a site with a very deep sitemap index reports fewer entries there than over
 * HTTP.
 */
export type MapResult = string[] & { sitemaps?: string[] };
/** Parse (`/v2/parse`) returns a v2 document (markdown/json/metadata). */
export type ParseResult = V2Document;
/** Batch scrape (`/v2/batch/scrape`) returns one v2 document per URL. */
export type BatchResult = V2Document[];
/** Native `/v1/extract` returns one result object per URL, in request order. */
export type ExtractResult = ExtractUrlResult[];
/**
 * Search results on the managed API: a flat list of hits, or a
 * `{ web, news, images }` grouping when the request set `sources`, with the
 * answer-mode siblings ({@link SearchAnswer}) attached alongside. Use
 * `Array.isArray(result)` to tell the flat and grouped shapes apart. A
 * self-hosted server (custom `apiUrl`) instead returns a {@link SearchResponseData}
 * envelope — cast to that type in self-host code.
 */
export type SearchResult = (SearchResultItem[] | GroupedSearchResults) & SearchAnswer;
/** Change-tracking diff (`/v1/change-tracking/diff`): the diff for the page. */
export type DiffResult = ChangeTrackingResult;

/** Firecrawl-compatible Research API options (cloud only). */
export interface ResearchSearchOptions {
  k?: number;
  authors?: string;
  categories?: string;
  from?: string;
  to?: string;
}

export interface ResearchReadOptions {
  /** When set, returns top passages answering this question instead of metadata. */
  query?: string;
  k?: number;
}

export interface ResearchSimilarOptions {
  /** Required by Firecrawl: natural-language ranking intent. */
  intent: string;
  mode?: "similar" | "citers" | "references";
  k?: number;
  rerank?: boolean;
  anchor?: string[];
}

/** Filters for `findData()` (`GET /v1/data/find`). */
export interface FindDataOptions {
  /** Words to match against source names and descriptions. */
  query?: string;
  /** Only sources in this category, e.g. `Research`. */
  category?: string;
  /** Only this provider, e.g. `openalex`. */
  provider?: string;
  /** Results per page (server default 10, max 50). */
  limit?: number;
  /** Results to skip; pass the previous page's `nextOffset`. */
  offset?: number;
}

/** One data source capability, as `findData()` lists it. */
export interface DataSource {
  provider: string;
  capability: string;
  name: string | null;
  description: string | null;
  /** The catalogue category, e.g. `Research`; `null` from an older server. */
  category: string | null;
  creditsCost: number | null;
  requiredOptions: string[];
  optionalOptions: string[];
  keyRequired: boolean;
  licence: string | null;
}

export interface FindDataResult {
  items: DataSource[];
  total: number | null;
  /** Present when another page exists. */
  nextOffset?: number;
}

/** Values for a source's `requiredOptions` and `optionalOptions`. */
export type DataOptions = Record<string, string | number | boolean | null | undefined>;

export interface DataProvenance {
  source: string | null;
  upstreamUrl: string | null;
  licence: string | null;
  licenceUrl: string | null;
  attributionText: string | null;
  retrievedAt: string | null;
}

/** `getData()` result: the source's record plus where it came from. */
export interface DataResult {
  provider: string;
  capability: string;
  creditsUsed: number | null;
  provenance: DataProvenance;
  data: unknown;
}
