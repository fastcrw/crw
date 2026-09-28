//! Shared MCP (Model Context Protocol) JSON-RPC types and tool definitions.
//!
//! Used by both the HTTP MCP endpoint (`crw-server`) and the stdio MCP proxy (`crw-mcp`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// MCP spec revision advertised in the `initialize` handshake (lib.rs `initialize`
/// arm). Bumped from "2024-11-05" to "2025-06-18" to legitimize tool `outputSchema`
/// and result `structuredContent`, both introduced in the 2025-06-18 revision.
/// There is no per-feature capability flag for structured output, so advertising
/// the revision that defines it is the only spec-legal way to emit it.
///
/// NOTE: `crw-browse` is a separate rmcp-based MCP server that pins its own
/// `ProtocolVersion::V_2024_11_05` (crw-browse/src/server.rs) and does NOT consume
/// this constant — it intentionally stays on 2024-11-05.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Server-level usage guidance returned in the `initialize` result's optional
/// `instructions` field (MCP InitializeResult). Clients surface this to the model
/// as "how to use this server", so it is the single sanctioned lever for steering
/// an agent to reach for these tools on web-shaped tasks. Kept factual (states the
/// tools' real capability + when they apply) — NOT a "always use instead of X"
/// directive, which reviewers and hosts penalize. Sits outside `tools/list`, so it
/// does not count against the tools/list token budget.
pub const SERVER_INSTRUCTIONS: &str = "fastCRW gives you live web access. Prefer these tools whenever a task needs information from the internet rather than answering from memory: crw_search for web search and current or real-time facts, crw_scrape to read a specific URL as clean markdown, crw_map to discover a site's URLs, crw_crawl to gather many pages across a site, and crw_extract to pull structured data from pages. When the user asks about recent, live, or source-specific information, reach for these instead of guessing.";

/// Variant used when no search backend is configured. `tools/list` strips
/// `crw_search` in that case, so the default instructions would name a tool the
/// client can never call — the two surfaces must agree.
pub const SERVER_INSTRUCTIONS_NO_SEARCH: &str = "fastCRW gives you live web access. Prefer these tools whenever a task needs information from the internet rather than answering from memory: crw_scrape to read a specific URL as clean markdown, crw_map to discover a site's URLs, crw_crawl to gather many pages across a site, and crw_extract to pull structured data from pages. When the user asks about recent, live, or source-specific information, reach for these instead of guessing.";

/// The `instructions` string that matches the tool set actually advertised.
pub fn server_instructions(search_available: bool) -> &'static str {
    if search_available {
        SERVER_INSTRUCTIONS
    } else {
        SERVER_INSTRUCTIONS_NO_SEARCH
    }
}

// --- JSON-RPC types ---

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// --- Tool definitions ---

fn extract_accepted_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "success": { "type": "boolean" },
            "id": { "type": "string" },
            "status": { "type": "string", "enum": ["processing"] },
            "urls": { "type": "integer", "minimum": 0 }
        },
        "required": ["success", "id", "status", "urls"]
    })
}

fn extract_status_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "success": { "type": "boolean" },
            "id": { "type": "string" },
            "status": {
                "type": "string",
                "enum": ["processing", "cancelling", "completed", "failed", "cancelled"]
            },
            "results": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "url": { "type": "string" },
                        "status": {
                            "type": "string",
                            "enum": ["processing", "completed", "failed", "cancelled"]
                        },
                        "data": { "type": "object", "additionalProperties": true },
                        "error": { "type": "string" },
                        "llmUsage": { "type": "object" },
                        "basis": { "type": "array", "items": { "type": "object" } },
                        "basisWarnings": { "type": "array", "items": { "type": "object" } },
                        "llmInputHash": { "type": "string" }
                    },
                    "required": ["url", "status"]
                }
            },
            "error": { "type": "string" },
            "expiresAt": { "type": "string", "format": "date-time" },
            "creditsUsed": { "type": "integer" },
            "tokensUsed": { "type": "integer" }
        },
        // `creditsUsed` is NOT required: `[mcp] hide_credits` (self-hosted
        // deployments, where credit bookkeeping is billing noise) strips it
        // from every tool response, and a strict client must still validate
        // the stripped body. It stays a declared property, so deployments
        // that do emit it validate identically. See [`strip_credit_fields`].
        "required": ["success", "id", "status", "results", "expiresAt", "tokensUsed"]
    })
}

pub fn tool_definitions(proxy_mode: bool) -> Value {
    let mut tools = vec![
        json!({
            "name": "crw_scrape",
            "title": "Scrape URL",
            "description": "Scrape one URL to markdown, HTML, or links.",
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to scrape"
                    },
                    "formats": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["markdown", "html", "links", "images"] },
                        "description": "Output formats (default [\"markdown\"])"
                    },
                    "onlyMainContent": {
                        "type": "boolean",
                        "description": "Strip nav/footer; main content only (default true)"
                    },
                    "includeTags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "CSS selectors to include"
                    },
                    "excludeTags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "CSS selectors to exclude"
                    },
                    "renderJs": {
                        "type": "boolean",
                        "description": "Force JS render (true), HTTP-only (false), omit = auto"
                    },
                    "waitFor": {
                        "type": "integer",
                        "description": "Ms to wait after JS render for late content"
                    },
                    "maxLength": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Max chars per content field; 0 = unbounded (default ~15000)"
                    },
                    "maxAge": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Max age of a cached copy of this fetch, in ms (default 3600000; 0 = always fetch; cap 24h)"
                    },
                    "renderer": {
                        "type": "string",
                        "enum": ["auto", "lightpanda", "chrome", "playwright", "camoufox", "impersonated-http"],
                        "description": "Pin renderer; browser tiers imply renderJs:true (default auto). 'camoufox' needs the opt-in tier configured. 'impersonated-http' is JS-less Chrome-TLS impersonation, never renderJs."
                    }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "crw_crawl",
            "title": "Crawl site",
            "description": "Start an async site crawl; returns a job id to poll with crw_check_crawl_status.",
            // Starting a crawl creates server-side job state (a side effect), so
            // this is NOT read-only and NOT idempotent.
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Starting URL"
                    },
                    "maxDepth": {
                        "type": "integer",
                        "description": "Max crawl depth (default 2)"
                    },
                    "maxPages": {
                        "type": "integer",
                        "description": "Max pages to crawl (default 10)"
                    },
                    "jsonSchema": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Optional. A JSON Schema (draft 2020-12) describing fields to extract from each page via an LLM, e.g. {\"type\":\"object\",\"properties\":{\"title\":{\"type\":\"string\"}}}. Free-form object. Omit to crawl without structured extraction."
                    },
                    "renderJs": {
                        "type": "boolean",
                        "description": "Force JS render (true), HTTP-only (false), omit = auto"
                    },
                    "waitFor": {
                        "type": "integer",
                        "description": "Ms to wait after JS render per page"
                    },
                    "renderer": {
                        "type": "string",
                        "enum": ["auto", "lightpanda", "chrome", "playwright", "camoufox", "impersonated-http"],
                        "description": "Pin renderer; browser tiers imply renderJs:true (default auto). 'camoufox' needs the opt-in tier configured. 'impersonated-http' is JS-less Chrome-TLS impersonation, never renderJs."
                    }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "crw_check_crawl_status",
            "title": "Check crawl status",
            "description": "Poll an async crawl job and retrieve its pages.",
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Crawl job id from crw_crawl"
                    },
                    "maxLength": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Max chars per page content field; 0 = unbounded (default ~15000)"
                    }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "crw_map",
            "title": "Map site URLs",
            "description": "Discover URLs on a site via sitemap and/or a short crawl. Returns a URL list only, no page content.",
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to map"
                    },
                    "maxDepth": {
                        "type": "integer",
                        "description": "Max discovery depth (default 2)"
                    },
                    "useSitemap": {
                        "type": "boolean",
                        "description": "Use sitemap.xml (default true)"
                    },
                    "crawlFallback": {
                        "type": "boolean",
                        "description": "Supplement sitemap with a short BFS crawl (default true; false = sitemap-only)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Max URLs to discover AND return; 0 = unbounded (default 100). Raise it (e.g. 50000) to pull deep/large sitemaps."
                    }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "crw_extract",
            "title": "Extract structured data",
            "description": "Extract structured JSON from URLs via a prompt and/or JSON schema. Async job — poll crw_check_extract_status with the returned id. Needs an LLM.",
            // Starting an extract creates server-side job state (a side effect),
            // so this is NOT read-only and NOT idempotent (same as crw_crawl).
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "URLs to extract from"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Free-text extraction objective (required unless schema is given)"
                    },
                    "schema": {
                        "type": "object",
                        "description": "JSON Schema constraining the extracted output"
                    },
                    "basis": {
                        "type": "boolean",
                        "description": "Return per-field evidence: each top-level scalar property comes back with a source url, verbatim excerpt and honest status (supported/unverified/unsupported/notFound). Requires schema."
                    },
                    "llmApiKey": { "type": "string", "description": "BYOK LLM API key" },
                    "llmProvider": { "type": "string" },
                    "llmModel": { "type": "string" }
                },
                "required": ["urls"]
            },
            "outputSchema": extract_accepted_output_schema()
        }),
        json!({
            "name": "crw_check_extract_status",
            "title": "Check extract job status",
            "description": "Poll an extract job; returns status and, when complete, a per-URL results array.",
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Extract job id from crw_extract" }
                },
                "required": ["id"]
            },
            "outputSchema": extract_status_output_schema()
        }),
        json!({
            "name": "crw_cancel_extract",
            "title": "Cancel extract job",
            "description": "Request cancellation of an extract job. Returns the canonical status; cancelling remains non-terminal until the claimed URL settles.",
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": true,
                "openWorldHint": true
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Extract job id from crw_extract" }
                },
                "required": ["id"]
            },
            "outputSchema": extract_status_output_schema()
        }),
    ];

    // `tool_definitions` always emits `crw_search`; whether the client SEES it is
    // decided one level up, in `handle_protocol_method`'s `tools/list` arm, which
    // retains it out when `search_available` is false (an embedded install with no
    // search backend configured). Proxy mode always has it: the remote decides.
    // The tool SET does not depend on the mode; the advertised output contract does
    // (see the `proxy_mode` strip below the last push).
    tools.push(json!({
        "name": "crw_search",
        "title": "Web search",
        "description": "Search the web for current information, news, facts, or docs. Use whenever the answer may depend on up-to-date or external information. Returns ranked results (url/title/description/snippet); optionally scrape each result inline.",
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": true
        },
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 5, max 20)"
                },
                "lang": {
                    "type": "string",
                    "description": "Language code, e.g. \"en\", \"tr\""
                },
                "tbs": {
                    "type": "string",
                    "enum": ["qdr:h", "qdr:d", "qdr:w", "qdr:m", "qdr:y"],
                    "description": "Time filter: past hour/day/week/month/year"
                },
                "sources": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["web", "news", "images"] },
                    "description": "If set, group results by source instead of a flat list"
                },
                "categories": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Category bias; e.g. \"pdf\", \"github\", \"research\", \"news\", \"images\""
                },
                "scrapeOptions": {
                    "type": "object",
                    "description": "If set, scrape each web result and inline the requested formats",
                    "properties": {
                        "formats": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["markdown", "html", "rawHtml", "links", "images"] }
                        },
                        "onlyMainContent": {
                            "type": "boolean",
                            "description": "Strip nav/footer/ads (default true)"
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Per-result scrape budget, ms (default 15000, max 60000)"
                        }
                    }
                }
            },
            "required": ["query"]
        },
        // Intentionally minimal: declares the stable top-level contract
        // (`{success, data:{results}}`) that strict clients validate, while leaving
        // `results` permissive — it is a `#[serde(untagged)]` enum that serializes
        // either as a flat array OR a grouped `{web,news,images}` object, and items
        // carry conditional fields (markdown/html/links/imageUrl/…). A rich schema
        // here costs ~400 tok in every `tools/list` for little client benefit and
        // risks falsely rejecting real responses, so we keep it skeletal. No
        // `additionalProperties:false` anywhere (conditional fields).
        "outputSchema": {
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "data": {
                    "type": "object",
                    "properties": {
                        "results": {
                            "oneOf": [
                                { "type": "array", "items": { "type": "object" } },
                                { "type": "object" }
                            ]
                        }
                    },
                    "required": ["results"]
                }
            },
            "required": ["success", "data"]
        }
    }));

    tools.push(json!({
        "name": "crw_parse_file",
        "title": "Parse PDF",
        "description": "Parse a local PDF (base64 in contentBase64) to markdown. No OCR: scanned PDFs return empty markdown with a warning.",
        // openWorldHint:false — operates on provided bytes, not the open web.
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "properties": {
                "contentBase64": {
                    "type": "string",
                    "description": "Base64-encoded PDF bytes"
                },
                "filename": {
                    "type": "string",
                    "description": "Original filename (optional)"
                },
                "formats": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["markdown", "plainText", "links", "images", "json", "summary"] },
                    "description": "Output formats (default [\"markdown\"]); json/summary need a server LLM"
                },
                "jsonSchema": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "Optional. A JSON Schema (draft 2020-12) describing fields to extract when formats includes \"json\", e.g. {\"type\":\"object\",\"properties\":{\"title\":{\"type\":\"string\"}}}. Free-form object."
                },
                "parsers": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["pdf"] },
                    "description": "Parsers to apply (default [\"pdf\"])"
                },
                "maxLength": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Max chars per content field; 0 = unbounded (default ~15000)"
                }
            },
            "required": ["contentBase64"]
        }
    }));

    // In proxy mode the body is whatever the REMOTE returns, and we do not control that
    // remote: `--api-url` may point at a self-hosted crw-server (which nests the results
    // under `data.results`) or at the managed API (whose public REST contract puts them
    // directly in `data`). A declared `outputSchema` is a promise about a body we do not
    // author, and the spec says a server MUST conform to a schema it declares. A strict
    // client then hard-fails the ENTIRE call when it does not: the official SDKs raise
    // `-32602 Structured content does not match the tool's output schema` (issue #391).
    // Stripping it is spec-legal — `outputSchema` is optional — and callers lose no data:
    // `structuredContent` is still emitted, it just carries no advertised schema for a
    // client to validate it against. The managed connector reached the same rule
    // independently for the same reason (crw-saas `src/lib/mcp/dispatch.ts`,
    // `managedSearchTool`).
    //
    // Applied to EVERY tool rather than a hand-picked list: "we do not author this body"
    // is true of all of them in proxy mode, and a per-tool list has to be re-audited on
    // every future shape change — which is exactly how `crw_extract` stayed broken after
    // `crw_search` was already known to be.
    //
    // Embedded/engine mode (`proxy_mode == false`) keeps its schemas: there the body is
    // ours and its shape is locked by tests.
    if proxy_mode {
        for tool in &mut tools {
            if let Some(obj) = tool.as_object_mut() {
                obj.remove("outputSchema");
            }
        }
    }

    json!({ "tools": tools })
}

/// Returns the ENGINE's declared `outputSchema` for a tool, if it declares one.
///
/// Reads `tool_definitions(false)` deliberately, and that asymmetry is the point:
/// it is the "does this tool have a structured shape at all" question, which drives
/// `structuredContent` emission in [`tool_result_response`]. In proxy mode the schema
/// is not *advertised* (we do not author the remote's body — see the strip in
/// `tool_definitions`), but the structured value is still emitted, unvalidated. Do
/// not "fix" this into taking `proxy_mode`: that would silently stop emitting
/// `structuredContent` on the proxy. Pinned by
/// `proxy_mode_still_emits_structured_content_without_a_schema`.
///
/// Recomputes `tool_definitions` per call — `tools/call` is not hot; memoize behind a
/// `OnceLock` only if profiling ever demands it.
pub fn tool_output_schema(tool_name: &str) -> Option<Value> {
    tool_definitions(false)["tools"]
        .as_array()?
        .iter()
        .find(|t| t["name"] == tool_name)
        .and_then(|t| t.get("outputSchema").cloned())
}

/// Whether `name` is one of the server's tool names. A genuinely unknown tool
/// should be answered with a JSON-RPC `-32602` protocol error (clients degrade
/// more gracefully than on an `isError` execution result). Checks the full set
/// regardless of runtime availability (e.g. `crw_search` is a known name even when
/// no search backend is configured — calling it then yields a clear runtime error).
pub fn is_known_tool(name: &str) -> bool {
    tool_definitions(false)["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|t| t["name"] == name))
}

/// Result of handling a protocol method.
pub enum ProtocolResult {
    /// Send this response back to the client.
    Response(JsonRpcResponse),
    /// Notification — no response needed.
    Notification,
    /// Not a protocol method — caller should handle it.
    NotHandled,
}

/// Handle common MCP protocol methods (initialize, tools/list, ping, notifications).
///
/// `search_available` controls whether `crw_search` is advertised in `tools/list`.
/// Proxy callers pass `true` (the remote decides); embedded callers pass whether a
/// search backend (SearXNG) is actually configured, so users who run `npx … crw`
/// with no backend don't see a tool that only ever returns `search_disabled`.
pub fn handle_protocol_method(
    server_name: &str,
    server_version: &str,
    req: &JsonRpcRequest,
    proxy_mode: bool,
    search_available: bool,
) -> ProtocolResult {
    if req.jsonrpc != "2.0" {
        let id = req.id.clone().unwrap_or(Value::Null);
        return ProtocolResult::Response(JsonRpcResponse::error(
            id,
            -32600,
            "invalid jsonrpc version".into(),
        ));
    }

    match req.method.as_str() {
        "notifications/initialized" | "notifications/cancelled" => ProtocolResult::Notification,

        "initialize" => {
            let id = req.id.clone().unwrap_or(Value::Null);
            ProtocolResult::Response(JsonRpcResponse::success(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    // The tool set is fixed for the lifetime of a session (it depends
                    // only on startup config), so we never emit tools/list_changed.
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": server_name,
                        "version": server_version
                    },
                    "instructions": server_instructions(search_available)
                }),
            ))
        }

        "tools/list" => {
            let id = req.id.clone().unwrap_or(Value::Null);
            let mut defs = tool_definitions(proxy_mode);
            if !search_available
                && let Some(tools) = defs.get_mut("tools").and_then(Value::as_array_mut)
            {
                tools.retain(|t| t["name"] != "crw_search");
            }
            ProtocolResult::Response(JsonRpcResponse::success(id, defs))
        }

        "ping" => {
            let id = req.id.clone().unwrap_or(Value::Null);
            ProtocolResult::Response(JsonRpcResponse::success(id, json!({})))
        }

        _ => ProtocolResult::NotHandled,
    }
}

/// Wrap a tool call result into an MCP-compliant content response.
///
/// On success the structured `value` is emitted **both** as a text content block
/// (verbatim, for backward compatibility with lenient clients and clients that
/// negotiated an older protocol revision) **and**, when the called tool has a
/// structured shape at all ([`tool_output_schema`]), as a top-level
/// `structuredContent` field (MCP 2025-06-18). Both representations derive from the
/// same `value` binding, so `serde_json::from_str(content[0].text) == structuredContent`
/// holds by construction — the two can never disagree.
///
/// Note the deliberate asymmetry in proxy mode: the schema is not advertised in
/// `tools/list` (see the `proxy_mode` strip in [`tool_definitions`]) yet
/// `structuredContent` is still emitted. That is spec-legal — the spec asks clients to
/// validate only when a schema was advertised — and it means proxy callers keep the
/// structured value without the hard failure that a promise we cannot keep would cause.
pub fn tool_result_response(
    id: Value,
    tool_name: &str,
    result: Result<Value, String>,
) -> JsonRpcResponse {
    match result {
        Ok(value) => {
            // Compact (not pretty) — pretty-printing adds ~30% whitespace, and this
            // text block is injected verbatim into the agent's context.
            let text = serde_json::to_string(&value).unwrap_or_default();
            let mut payload = json!({
                "content": [{"type": "text", "text": text}]
            });
            // Attach structuredContent only when (a) the tool declares an
            // outputSchema and (b) the value is a JSON object — the spec requires
            // structuredContent to be an object. The `is_object()` guard is the
            // proxy version-skew safety valve: in proxy mode a schema-bearing tool
            // may yield a non-object Ok value (an upstream error string, a plain
            // string, or a legacy top-level array) — degrade to text-only rather
            // than ship a spec-violating structuredContent to a strict client.
            // Locked by test T2b. Do NOT remove the is_object() guard.
            if value.is_object() && tool_output_schema(tool_name).is_some() {
                payload["structuredContent"] = value;
            }
            JsonRpcResponse::success(id, payload)
        }
        // Err path: never attach structuredContent. `isError:true` signals
        // failure, and strict clients must not validate outputSchema against an
        // error result.
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{"type": "text", "text": e}],
                "isError": true
            }),
        ),
    }
}

// --- Output bounding (MCP-layer, context-footprint control) ---

/// Default per-content-field char cap for scrape/parse/crawl-status results.
/// ~15K chars ≈ ~3.5–4K tokens — well under the typical ~25K-token client cap.
pub const DEFAULT_MAX_LENGTH: usize = 15_000;
/// Default cap on the number of URLs `crw_map` returns to the model.
pub const DEFAULT_MAP_LIMIT: usize = 100;

/// Large string fields on a serialized `ScrapeData` (camelCase) worth truncating.
const SCRAPE_TEXT_FIELDS: &[&str] = &["markdown", "html", "rawHtml", "plainText", "summary"];

/// Resolve an MCP-only bound argument. Returns:
/// - `Some(default)` when the arg is absent,
/// - `None` (= unbounded) when the arg is explicitly `0`,
/// - `Some(n)` for a positive value.
fn resolve_bound(args: &Value, key: &str, default: usize) -> Option<usize> {
    match args.get(key).and_then(Value::as_u64) {
        None => Some(default),
        Some(0) => None,
        Some(n) => Some(n as usize),
    }
}

/// Truncate a string to at most `max_chars` characters on a char boundary,
/// appending a visible marker. Returns `None` if no truncation was needed.
fn truncate_to_chars(s: &str, max_chars: usize) -> Option<String> {
    // `nth(max_chars)` yields the (max_chars+1)-th char; its byte offset is where
    // we cut to keep exactly `max_chars` chars. Absent → string is short enough.
    s.char_indices()
        .nth(max_chars)
        .map(|(byte_idx, _)| format!("{}\n…[truncated by crw-mcp maxLength]", &s[..byte_idx]))
}

/// Truncate the known large text fields of one serialized `ScrapeData` object,
/// tagging it with `truncated: true` if anything was cut. Non-recursive.
fn truncate_scrape_obj(value: &mut Value, max: usize) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let mut any = false;
    for field in SCRAPE_TEXT_FIELDS {
        let cut = match obj.get(*field) {
            Some(Value::String(s)) => truncate_to_chars(s, max),
            _ => None,
        };
        if let Some(t) = cut {
            obj.insert((*field).to_string(), Value::String(t));
            any = true;
        }
    }
    if any {
        obj.insert("truncated".to_string(), Value::Bool(true));
    }
}

/// The single `ScrapeData`-shaped object to truncate. The **embedded** backend
/// returns the bare `ScrapeData` (fields at the top level); the **proxy** backend
/// forwards the REST `ApiResponse<ScrapeData>` envelope (`{success, data:{…}}`).
/// We unwrap the `data` envelope when present so both shapes are bounded identically.
fn scrape_target_mut(value: &mut Value) -> Option<&mut Value> {
    if value.get("data").is_some_and(Value::is_object) {
        value.get_mut("data")
    } else if value.is_object() {
        Some(value)
    } else {
        None
    }
}

/// Truncate the `links` and `sitemaps` lists to `limit` with markers, wherever
/// they live: top-level (embedded `{success, links}`) or under the `data`
/// envelope (proxy `ApiResponse<MapData>` = `{success, data:{links}}`). The two
/// lists are bounded independently, so a short one does not exempt a long one.
fn bound_map_links(value: &mut Value, limit: usize) {
    let in_envelope = value.get("data").and_then(|d| d.get("links")).is_some();
    let Some(container) = (if in_envelope {
        value.get_mut("data")
    } else {
        Some(&mut *value)
    }) else {
        return;
    };
    let total = container
        .get("links")
        .and_then(Value::as_array)
        .map(Vec::len);
    // `sitemaps` shares this bound: a site with a deep sitemap index can list
    // thousands of them, and letting that through unbounded would defeat the
    // whole point of capping `links` for the model's context.
    let total_sitemaps = container
        .get("sitemaps")
        .and_then(Value::as_array)
        .map(Vec::len);
    let links_over = total.is_some_and(|t| t > limit);
    let sitemaps_over = total_sitemaps.is_some_and(|t| t > limit);
    if !links_over && !sitemaps_over {
        return;
    }
    if let Some(obj) = container.as_object_mut() {
        if links_over {
            if let Some(Value::Array(links)) = obj.get_mut("links") {
                links.truncate(limit);
            }
            obj.insert("totalDiscovered".to_string(), json!(total));
        }
        if sitemaps_over {
            if let Some(Value::Array(sitemaps)) = obj.get_mut("sitemaps") {
                sitemaps.truncate(limit);
            }
            obj.insert("totalSitemaps".to_string(), json!(total_sitemaps));
        }
        obj.insert("truncated".to_string(), Value::Bool(true));
    }
}

/// Truncate any scrape content inlined into `crw_search` results (via
/// `scrapeOptions`). The results are either a flat array of items or a grouped
/// `{web,news,images}` object of arrays, and they live in one of two places: under
/// `data.results` on the engine's own envelope, or **directly** in `data` on the
/// managed REST contract, which hoists them one level. Bound both — the proxy fronts
/// either, and on the shape this helper did not handle the default bound was a silent
/// no-op that let whole scraped pages through (issue #391).
fn bound_search_results(value: &mut Value, max: usize) {
    let Some(data) = value.get_mut("data") else {
        return;
    };
    let results = if data.get("results").is_some() {
        data.get_mut("results").expect("presence checked above")
    } else {
        data
    };
    match results {
        Value::Array(items) => {
            for item in items.iter_mut() {
                truncate_scrape_obj(item, max);
            }
        }
        Value::Object(groups) => {
            for arr in groups.values_mut() {
                if let Some(items) = arr.as_array_mut() {
                    for item in items.iter_mut() {
                        truncate_scrape_obj(item, max);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Bound a tool result's size at the MCP layer, driven by the call's own
/// `maxLength`/`limit` arguments (see [`resolve_bound`] for the `0 = unbounded`
/// opt-out). **Non-mutating** w.r.t. any stored state: it transforms an owned
/// `Value` produced by the dispatch and returns a new one. Shared by the embedded,
/// proxy, and CLI paths, and handles BOTH the bare (embedded) and `ApiResponse`-
/// enveloped (proxy) result shapes so the two behave identically.
pub fn apply_bounds(tool_name: &str, args: &Value, mut value: Value) -> Value {
    match tool_name {
        "crw_scrape" | "crw_parse_file" => {
            if let Some(max) = resolve_bound(args, "maxLength", DEFAULT_MAX_LENGTH)
                && let Some(target) = scrape_target_mut(&mut value)
            {
                truncate_scrape_obj(target, max);
            }
        }
        "crw_check_crawl_status" => {
            // CrawlState is returned bare (top-level `data` array) by both the
            // embedded backend and the REST `GET /v1/crawl/{id}` endpoint.
            if let Some(max) = resolve_bound(args, "maxLength", DEFAULT_MAX_LENGTH)
                && let Some(pages) = value.get_mut("data").and_then(Value::as_array_mut)
            {
                for page in pages.iter_mut() {
                    truncate_scrape_obj(page, max);
                }
            }
        }
        "crw_map" => {
            if let Some(limit) = resolve_bound(args, "limit", DEFAULT_MAP_LIMIT) {
                bound_map_links(&mut value, limit);
            }
        }
        "crw_search" => {
            if let Some(max) = resolve_bound(args, "maxLength", DEFAULT_MAX_LENGTH) {
                bound_search_results(&mut value, max);
            }
        }
        _ => {}
    }
    value
}

/// Remove MCP-only control args (`maxLength`) before a proxy forwards the call
/// to a REST endpoint that may reject unknown body fields. These are applied
/// locally via [`apply_bounds`] on the response instead.
///
/// `crw_map`'s `limit` and `crw_search`'s `limit` are *real* backend params and
/// are intentionally NOT stripped: `/v1/map` now drives sitemap discovery depth
/// from `limit`, so forwarding it lets a deliberate large limit actually find
/// (not just slice) more URLs. `apply_bounds` still caps the response.
///
/// `crw_search` strips `maxLength` even though its `inputSchema` does not advertise
/// the knob: `apply_bounds` honours it for every tool (see [`resolve_bound`]), so a
/// hand-written client can still send one, and it must not reach the REST body.
/// Advertising it on `crw_search` would push `tools/list` past the token ceiling
/// guarded by `tools_list_token_budget`; search results are bounded by the default
/// either way.
pub fn strip_mcp_only_args(tool_name: &str, mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        match tool_name {
            "crw_scrape" | "crw_parse_file" | "crw_check_crawl_status" | "crw_search" => {
                obj.remove("maxLength");
            }
            _ => {}
        }
    }
    args
}

// --- Credit-field stripping (MCP-layer, context-footprint control) ---

/// Serialized keys that carry credit-billing bookkeeping. `creditCost` is the
/// camelCase form of `ScrapeData.credit_cost` (per-page price); `creditsUsed`
/// is the aggregate billed on job/envelope responses (extract status, v2
/// scrape/crawl/search envelopes and their `metadata` blocks). Both exist for
/// the managed SaaS billing layer; on a self-hosted deployment they are dead
/// weight in the model's context.
const CREDIT_FIELDS: &[&str] = &["creditCost", "creditsUsed"];

/// Recursively remove [`CREDIT_FIELDS`] keys from every object in `value`.
///
/// Recursive rather than layout-driven (unlike [`apply_bounds`], which
/// navigates known shapes): credit keys appear at the top level
/// (`ScrapeData.creditCost`), inside a `data` envelope page array, and under v2
/// `metadata` — one rule covers current and future placements, and unknown
/// shapes pass through untouched.
///
/// Tool-agnostic for the same reason, and deliberately **not** driven by
/// `tool_name`. Nothing confines this to a self-hosted upstream: pointed at a
/// managed API, `--hide-credits` hides that account's real credit spend, which
/// is the operator's explicit choice and why the default is off. `tokensUsed`
/// is kept — it measures real LLM provider consumption, useful to a
/// self-hosted operator tuning prompts, not SaaS credit bookkeeping.
///
/// Caller-shaped payloads are exempt: `ScrapeData.json`, `V2Document.json`,
/// `changeTracking.snapshot.json` / `.diff.json`, and an extract
/// `results[].data` are filled from the caller's own `jsonSchema`, so a key
/// named `creditCost` in there is the caller's extracted value, not our
/// bookkeeping. Removing it would be silent data loss in the one part of the
/// response the engine does not author. The `json`-key rule is what covers the
/// first four; do not narrow it to a fixed path list. `metadata` is still walked — it carries
/// the engine's own v2 credit block, and a page whose raw `<meta>` tags flatten
/// to a key named `creditCost` is not a shape worth losing that to.
///
/// Caller contract: apply to the tool result `Value` *before*
/// [`tool_result_response`] wraps it, so the stripped shape lands in both the
/// text content block and `structuredContent`.
pub fn strip_credit_fields(value: &mut Value) {
    strip_credit_fields_inner(value, false);
}

/// `in_results` marks that `value` sits under an extract `results` array, where
/// the `data` key holds caller-shaped extraction output rather than a crawl
/// page. A top-level `data` envelope (crawl/batch page array) is still walked.
fn strip_credit_fields_inner(value: &mut Value, in_results: bool) {
    match value {
        Value::Object(map) => {
            for field in CREDIT_FIELDS {
                map.remove(*field);
            }
            for (key, v) in map.iter_mut() {
                if key == "json" || (in_results && key == "data") {
                    continue;
                }
                strip_credit_fields_inner(v, key == "results");
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                strip_credit_fields_inner(v, in_results);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_by_name<'a>(tools: &'a Value, name: &str) -> &'a Value {
        tools["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} not found"))
    }

    /// Token-budget regression gate for the `tools/list` payload. Every byte here
    /// is injected into the agent's context on every turn, so this is the server's
    /// single most important footprint metric.
    ///
    /// We estimate tokens as `ceil(bytes / 3)` — a deliberately *conservative*
    /// (over-counting) heuristic: symbol-heavy JSON tokenizes at ~3–4 chars/token,
    /// so if this estimate is under the ceiling the real (tiktoken/cl100k) count is
    /// comfortably under too. A real tokenizer (`tiktoken-rs`) was considered but
    /// rejected to keep this leaf crate dependency-free; the conservative estimate
    /// is sufficient for a regression gate. Real cl100k count is ~25–30% lower.
    ///
    /// Baseline before the Phase 1 trim was 8233 bytes (~2744 est-tok). After the
    /// Phase 1 trim + Phase 3 annotations/titles + the two native extract tools the
    /// full 8-tool list was ~8017 bytes (~2673 est-tok). The canonical lifecycle
    /// adds one cancel tool plus required output schemas for start/status/cancel;
    /// after closing lifecycle statuses and typing every per-URL result field,
    /// the 9-tool list is ~10705 bytes (~3569 est-tok). The ceiling keeps ~2%
    /// headroom so further growth still fails.
    ///
    /// Raised 3650 -> 3700 for the sixth `renderer` enum value
    /// ("impersonated-http", both scrape and crawl schemas) plus the one-line
    /// description note that it is JS-less; the wording itself was trimmed in
    /// the same change (the naive description growth alone would have been
    /// ~3726 est-tok).
    ///
    /// Raised 3700 -> 3750 for the `maxAge` parameter on `crw_scrape`: the
    /// fetch cache ships default-on (1h reuse), so MCP callers need the opt-out
    /// lever in the schema. The description is one line; the remaining delta is
    /// the property itself (trimmed footprint ~3727 est-tok).
    const TOOLS_LIST_TOKEN_CEILING: usize = 3750;

    #[test]
    fn tools_list_token_budget() {
        let json = serde_json::to_string(&tool_definitions(false)).unwrap();
        let est_tokens = json.len().div_ceil(3);
        assert!(
            est_tokens <= TOOLS_LIST_TOKEN_CEILING,
            "tools/list footprint regressed: {} bytes ≈ {} est-tokens (ceiling {}). \
             Trim descriptions/schemas before raising the ceiling.",
            json.len(),
            est_tokens,
            TOOLS_LIST_TOKEN_CEILING
        );
    }

    #[test]
    fn crw_scrape_schema_advertises_render_js() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let props = &scrape["inputSchema"]["properties"];
        assert_eq!(
            props["renderJs"]["type"], "boolean",
            "renderJs must be a plain boolean in the advertised schema"
        );
        assert!(
            props["renderJs"].get("default").is_none(),
            "renderJs must not advertise a default — server resolves it"
        );
    }

    #[test]
    fn crw_scrape_schema_advertises_wait_for() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let props = &scrape["inputSchema"]["properties"];
        assert_eq!(props["waitFor"]["type"], "integer");
    }

    #[test]
    fn crw_scrape_schema_advertises_max_age() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let props = &scrape["inputSchema"]["properties"];
        assert_eq!(
            props["maxAge"]["type"], "integer",
            "maxAge must be advertised so MCP callers can opt out of the 1h fetch cache"
        );
        assert_eq!(props["maxAge"]["minimum"], 0);
        assert!(
            props["maxAge"].get("default").is_none(),
            "maxAge must not advertise a default: the server default (1h) is the documented one"
        );
    }

    #[test]
    fn crw_scrape_render_js_not_required() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let required = scrape["inputSchema"]["required"]
            .as_array()
            .expect("required array");
        assert!(
            !required.iter().any(|v| v == "renderJs"),
            "renderJs must not be in required"
        );
        assert!(
            !required.iter().any(|v| v == "waitFor"),
            "waitFor must not be in required"
        );
    }

    #[test]
    fn crw_crawl_schema_advertises_render_js_and_wait_for() {
        let defs = tool_definitions(false);
        let crawl = tool_by_name(&defs, "crw_crawl");
        let props = &crawl["inputSchema"]["properties"];
        assert_eq!(props["renderJs"]["type"], "boolean");
        assert_eq!(props["waitFor"]["type"], "integer");
    }

    #[test]
    fn crw_scrape_schema_advertises_renderer() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let props = &scrape["inputSchema"]["properties"];
        assert_eq!(props["renderer"]["type"], "string");
        let enum_vals = props["renderer"]["enum"]
            .as_array()
            .expect("renderer.enum must be an array");
        assert_eq!(
            enum_vals,
            &vec![
                json!("auto"),
                json!("lightpanda"),
                json!("chrome"),
                json!("playwright"),
                json!("camoufox"),
                json!("impersonated-http"),
            ]
        );
    }

    #[test]
    fn crw_scrape_renderer_not_required() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let required = scrape["inputSchema"]["required"]
            .as_array()
            .expect("required array");
        assert!(!required.iter().any(|v| v == "renderer"));
    }

    #[test]
    fn crw_crawl_schema_advertises_renderer() {
        let defs = tool_definitions(false);
        let crawl = tool_by_name(&defs, "crw_crawl");
        let props = &crawl["inputSchema"]["properties"];
        assert_eq!(props["renderer"]["type"], "string");
        let enum_vals = props["renderer"]["enum"]
            .as_array()
            .expect("renderer.enum must be an array");
        assert_eq!(enum_vals.len(), 6);
        assert!(enum_vals.iter().any(|v| v == "chrome"));
        assert!(enum_vals.iter().any(|v| v == "lightpanda"));
        assert!(enum_vals.iter().any(|v| v == "auto"));
        assert!(enum_vals.iter().any(|v| v == "playwright"));
        assert!(enum_vals.iter().any(|v| v == "camoufox"));
        assert!(enum_vals.iter().any(|v| v == "impersonated-http"));
    }

    #[test]
    fn schemas_do_not_set_additional_properties_false() {
        // Deferred to a follow-up issue. Guard against accidentally enabling
        // this before the schemas are expanded to full ScrapeRequest parity.
        let defs = tool_definitions(false);
        for name in ["crw_scrape", "crw_crawl", "crw_map"] {
            let tool = tool_by_name(&defs, name);
            let ap = &tool["inputSchema"].get("additionalProperties");
            assert!(
                ap.is_none() || ap.as_ref().and_then(|v| v.as_bool()) != Some(false),
                "{name}: additionalProperties:false must remain off until schemas are complete"
            );
        }
    }

    // --- structuredContent emission (issue #89) ---

    /// A single text-result item with every always-emitted field set, plus the
    /// optional `score`/`category`. `snippet` mirrors `description`, matching the
    /// real `SearchResult` serializer (snippet is an alias of description).
    fn search_result_item(idx: u32) -> Value {
        json!({
            "url": format!("https://example.com/{idx}"),
            "title": format!("Result {idx}"),
            "description": "body text",
            "snippet": "body text",
            "position": idx,
            "score": 4.0,
            "category": "general"
        })
    }

    /// A representative flat (`sources` unset) crw_search success value, shaped
    /// like `ApiResponse::ok(SearchResponseData { results: Flat(..), .. })`.
    fn representative_search_value() -> Value {
        json!({
            "success": true,
            "data": { "results": [search_result_item(1), search_result_item(2)] }
        })
    }

    /// A representative grouped (`sources` set) value: `results` is an object with
    /// `web`/`news` (text items) and `images` (the differently-shaped ImageResult).
    fn grouped_search_value() -> Value {
        json!({
            "success": true,
            "data": { "results": {
                "web": [search_result_item(1)],
                "news": [search_result_item(2)],
                "images": [{
                    "url": "https://example.com/img",
                    "title": "An image",
                    "description": "alt text",
                    "imageUrl": "https://example.com/img.png",
                    "position": 1
                }]
            }}
        })
    }

    fn result_of(resp: &JsonRpcResponse) -> &Value {
        resp.result.as_ref().expect("success response has result")
    }

    /// T1 — crw_search Ok emits BOTH a text block and structuredContent, and the
    /// two are byte-for-byte the same value (single-source invariant).
    #[test]
    fn t1_search_emits_dual_content_in_sync() {
        let repr = representative_search_value();
        let resp = tool_result_response(json!(1), "crw_search", Ok(repr.clone()));
        let result = result_of(&resp);

        let text = result["content"][0]["text"]
            .as_str()
            .expect("text content present");
        assert_eq!(
            result["content"][0]["type"], "text",
            "first content block is text"
        );

        let structured = &result["structuredContent"];
        assert!(!structured.is_null(), "structuredContent present");
        assert_eq!(
            structured, &repr,
            "structuredContent is the unmodified value"
        );

        let from_text: Value = serde_json::from_str(text).expect("text is valid JSON");
        assert_eq!(
            &from_text, structured,
            "from_str(content.text) == structuredContent (no drift)"
        );
    }

    /// T2 — a tool WITHOUT an outputSchema (crw_scrape) gets text only, no
    /// structuredContent (schema-gated emission).
    #[test]
    fn t2_scrape_has_no_structured_content() {
        let resp = tool_result_response(json!(1), "crw_scrape", Ok(json!({"markdown": "hi"})));
        let result = result_of(&resp);
        assert!(result["content"][0]["text"].is_string());
        assert!(
            result.get("structuredContent").is_none(),
            "crw_scrape declares no outputSchema → no structuredContent"
        );
    }

    /// T2b — proxy version-skew safety valve: a schema-bearing tool whose Ok
    /// value is NOT an object (upstream error string, or a legacy top-level
    /// array) degrades to text-only. Locks the is_object() guard.
    #[test]
    fn t2b_non_object_search_value_degrades_to_text() {
        for non_object in [json!("upstream error string"), json!([{ "url": "x" }])] {
            let resp = tool_result_response(json!(1), "crw_search", Ok(non_object.clone()));
            let result = result_of(&resp);
            assert!(
                result["content"][0]["text"].is_string(),
                "text block carries the body"
            );
            assert!(
                result.get("structuredContent").is_none(),
                "non-object Ok value must NOT emit structuredContent: {non_object}"
            );
        }
    }

    /// T3 — the Err path is an isError text result with no structuredContent.
    #[test]
    fn t3_error_path_has_no_structured_content() {
        let resp = tool_result_response(json!(1), "crw_search", Err("boom".into()));
        let result = result_of(&resp);
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "boom");
        assert!(result.get("structuredContent").is_none());
    }

    /// T4 — emitted structuredContent validates against the declared outputSchema
    /// for both the flat and the grouped value (using the same builders the
    /// real serializer would feed).
    #[test]
    fn t4_emitted_structured_content_validates_against_schema() {
        let schema = tool_output_schema("crw_search").expect("crw_search has outputSchema");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");

        for value in [representative_search_value(), grouped_search_value()] {
            let resp = tool_result_response(json!(1), "crw_search", Ok(value.clone()));
            let structured = result_of(&resp)["structuredContent"].clone();
            let errors: Vec<String> = validator
                .iter_errors(&structured)
                .map(|e| e.to_string())
                .collect();
            assert!(
                errors.is_empty(),
                "structuredContent failed schema validation for {value}:\n{}",
                errors.join("\n")
            );
        }
    }

    /// T5 — the helper is the single source of truth: present for crw_search,
    /// absent for crw_scrape, with the expected required-field structure.
    #[test]
    fn t5_tool_output_schema_helper() {
        let schema = tool_output_schema("crw_search").expect("crw_search has outputSchema");
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().expect("required array");
        assert_eq!(required, &vec![json!("success"), json!("data")]);
        assert_eq!(schema["properties"]["data"]["type"], "object");
        let data_required = schema["properties"]["data"]["required"]
            .as_array()
            .expect("data.required array");
        assert!(data_required.iter().any(|v| v == "results"));

        assert!(
            tool_output_schema("crw_scrape").is_none(),
            "crw_scrape declares no outputSchema"
        );
    }

    /// T6 — the additionalProperties:false guard is scoped to inputSchema only;
    /// the new outputSchema must not set it (the conditional SearchResult fields
    /// would make it falsely reject real responses).
    #[test]
    fn t6_output_schema_does_not_set_additional_properties_false() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let ap = search["outputSchema"].get("additionalProperties");
        assert!(
            ap.is_none() || ap.and_then(|v| v.as_bool()) != Some(false),
            "crw_search outputSchema must not set additionalProperties:false"
        );
    }

    // --- Output bounding (apply_bounds / strip_mcp_only_args) ---

    fn long_md(chars: usize) -> String {
        "x".repeat(chars)
    }

    /// B1 — crw_scrape truncates markdown past the default cap and tags `truncated`.
    #[test]
    fn b1_scrape_truncates_to_default_max_length() {
        let value =
            json!({ "markdown": long_md(DEFAULT_MAX_LENGTH + 500), "url": "https://e.com" });
        let out = apply_bounds("crw_scrape", &json!({}), value);
        let md = out["markdown"].as_str().unwrap();
        assert!(
            md.chars().count() <= DEFAULT_MAX_LENGTH + 40,
            "truncated to ~cap + marker"
        );
        assert!(md.contains("[truncated"), "marker present");
        assert_eq!(out["truncated"], json!(true));
    }

    /// B2 — short content is untouched and gets no `truncated` flag.
    #[test]
    fn b2_scrape_short_content_untouched() {
        let value = json!({ "markdown": "hello", "url": "https://e.com" });
        let out = apply_bounds("crw_scrape", &json!({}), value);
        assert_eq!(out["markdown"], json!("hello"));
        assert!(out.get("truncated").is_none());
    }

    /// B3 — explicit `maxLength: 0` opts out of bounding (unbounded).
    #[test]
    fn b3_scrape_max_length_zero_is_unbounded() {
        let big = long_md(DEFAULT_MAX_LENGTH * 2);
        let value = json!({ "markdown": big.clone() });
        let out = apply_bounds("crw_scrape", &json!({ "maxLength": 0 }), value);
        assert_eq!(
            out["markdown"].as_str().unwrap().chars().count(),
            big.chars().count()
        );
        assert!(out.get("truncated").is_none());
    }

    /// B4 — a custom `maxLength` is honored.
    #[test]
    fn b4_scrape_custom_max_length() {
        let value = json!({ "markdown": long_md(100) });
        let out = apply_bounds("crw_scrape", &json!({ "maxLength": 10 }), value);
        let md = out["markdown"].as_str().unwrap();
        assert!(md.starts_with(&"x".repeat(10)));
        assert!(md.contains("[truncated"));
    }

    /// B5 — crw_map truncates the links list to the default limit with markers.
    #[test]
    fn b5_map_truncates_links_to_limit() {
        let links: Vec<Value> = (0..250)
            .map(|i| json!(format!("https://e.com/{i}")))
            .collect();
        let value = json!({ "success": true, "links": links });
        let out = apply_bounds("crw_map", &json!({}), value);
        assert_eq!(out["links"].as_array().unwrap().len(), DEFAULT_MAP_LIMIT);
        assert_eq!(out["totalDiscovered"], json!(250));
        assert_eq!(out["truncated"], json!(true));
    }

    /// B5b — the sitemaps list shares the map bound, and a short links list
    /// does not exempt a long sitemaps list from it.
    #[test]
    fn b5b_map_bounds_sitemaps_independently_of_links() {
        let sitemaps: Vec<Value> = (0..250)
            .map(|i| json!(format!("https://e.com/sitemap-{i}.xml")))
            .collect();
        let value = json!({
            "success": true,
            "links": ["https://e.com/"],
            "sitemaps": sitemaps,
        });
        let out = apply_bounds("crw_map", &json!({}), value);
        assert_eq!(out["sitemaps"].as_array().unwrap().len(), DEFAULT_MAP_LIMIT);
        assert_eq!(out["totalSitemaps"], json!(250));
        assert_eq!(out["truncated"], json!(true));
        // links was under the limit, so it is untouched and unmarked.
        assert_eq!(out["links"].as_array().unwrap().len(), 1);
        assert!(out.get("totalDiscovered").is_none());
    }

    /// B6 — crw_map `limit: 0` returns all links, no markers.
    #[test]
    fn b6_map_limit_zero_is_unbounded() {
        let links: Vec<Value> = (0..250)
            .map(|i| json!(format!("https://e.com/{i}")))
            .collect();
        let value = json!({ "links": links });
        let out = apply_bounds("crw_map", &json!({ "limit": 0 }), value);
        assert_eq!(out["links"].as_array().unwrap().len(), 250);
        assert!(out.get("truncated").is_none());
    }

    /// B7 — crw_check_crawl_status truncates each page in `data`.
    #[test]
    fn b7_crawl_status_truncates_each_page() {
        let value = json!({
            "status": "completed",
            "data": [
                { "markdown": long_md(DEFAULT_MAX_LENGTH + 100), "url": "https://e.com/1" },
                { "markdown": "short", "url": "https://e.com/2" }
            ]
        });
        let out = apply_bounds("crw_check_crawl_status", &json!({}), value);
        let pages = out["data"].as_array().unwrap();
        assert_eq!(pages[0]["truncated"], json!(true));
        assert!(
            pages[0]["markdown"]
                .as_str()
                .unwrap()
                .contains("[truncated")
        );
        assert!(pages[1].get("truncated").is_none());
        assert_eq!(pages[1]["markdown"], json!("short"));
    }

    /// B8 — truncation cuts on a char boundary (no panic on multibyte input).
    #[test]
    fn b8_truncation_is_char_safe() {
        let value = json!({ "markdown": "é".repeat(100) });
        let out = apply_bounds("crw_scrape", &json!({ "maxLength": 10 }), value);
        // Must not panic and must keep exactly 10 'é' chars before the marker.
        assert!(
            out["markdown"]
                .as_str()
                .unwrap()
                .starts_with(&"é".repeat(10))
        );
    }

    /// B9 — strip removes MCP-only args per tool, but keeps crw_search's real `limit`.
    #[test]
    fn b9_strip_mcp_only_args() {
        let scrape = strip_mcp_only_args("crw_scrape", json!({ "url": "u", "maxLength": 100 }));
        assert!(scrape.get("maxLength").is_none());
        assert_eq!(scrape["url"], json!("u"));

        // crw_map.limit now drives backend discovery — must NOT be stripped.
        let map = strip_mcp_only_args("crw_map", json!({ "url": "u", "limit": 50 }));
        assert_eq!(map["limit"], json!(50));

        // crw_search.limit is a real backend param — must NOT be stripped. Its
        // maxLength, like crw_scrape's, is MCP-only and must be.
        let search = strip_mcp_only_args(
            "crw_search",
            json!({ "query": "q", "limit": 5, "maxLength": 100 }),
        );
        assert_eq!(search["limit"], json!(5));
        assert!(search.get("maxLength").is_none());
    }

    /// B10 — unknown/other tools pass through apply_bounds unchanged.
    #[test]
    fn b10_unknown_tool_passthrough() {
        let value = json!({ "anything": [1, 2, 3] });
        let out = apply_bounds("crw_crawl", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    /// B11 — PROXY shape: crw_scrape `ApiResponse<ScrapeData>` envelope
    /// (`{success, data:{markdown}}`) is truncated under `data`, not skipped.
    #[test]
    fn b11_scrape_proxy_envelope_is_bounded() {
        let value = json!({
            "success": true,
            "data": { "markdown": long_md(DEFAULT_MAX_LENGTH + 500), "url": "https://e.com" }
        });
        let out = apply_bounds("crw_scrape", &json!({}), value);
        let md = out["data"]["markdown"].as_str().unwrap();
        assert!(
            md.contains("[truncated"),
            "proxy-enveloped scrape must be bounded"
        );
        assert_eq!(out["data"]["truncated"], json!(true));
    }

    /// B12 — PROXY shape: crw_map `ApiResponse<MapData>` envelope
    /// (`{success, data:{links}}`) is truncated under `data`.
    #[test]
    fn b12_map_proxy_envelope_is_bounded() {
        let links: Vec<Value> = (0..250)
            .map(|i| json!(format!("https://e.com/{i}")))
            .collect();
        let value = json!({ "success": true, "data": { "links": links } });
        let out = apply_bounds("crw_map", &json!({}), value);
        assert_eq!(
            out["data"]["links"].as_array().unwrap().len(),
            DEFAULT_MAP_LIMIT
        );
        assert_eq!(out["data"]["totalDiscovered"], json!(250));
        assert_eq!(out["data"]["truncated"], json!(true));
    }

    /// A1 — every tool advertises annotations + a title; crw_crawl and crw_extract
    /// are non-idempotent, while cancel is destructive but idempotent.
    #[test]
    fn a1_tools_advertise_annotations_and_title() {
        let defs = tool_definitions(false);
        for t in defs["tools"].as_array().unwrap() {
            assert!(t["annotations"].is_object(), "{} annotations", t["name"]);
            assert!(t["title"].is_string(), "{} title", t["name"]);
            assert!(t["annotations"]["destructiveHint"].is_boolean());
        }
        let crawl = tool_by_name(&defs, "crw_crawl");
        assert_eq!(crawl["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(crawl["annotations"]["idempotentHint"], json!(false));
        // crw_extract also starts a job — must be non-read-only, non-idempotent.
        let extract = tool_by_name(&defs, "crw_extract");
        assert_eq!(extract["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(extract["annotations"]["idempotentHint"], json!(false));
        let cancel = tool_by_name(&defs, "crw_cancel_extract");
        assert_eq!(cancel["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(cancel["annotations"]["destructiveHint"], json!(true));
        assert_eq!(cancel["annotations"]["idempotentHint"], json!(true));
        let scrape = tool_by_name(&defs, "crw_scrape");
        assert_eq!(scrape["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(scrape["annotations"]["openWorldHint"], json!(true));
        let parse = tool_by_name(&defs, "crw_parse_file");
        assert_eq!(parse["annotations"]["openWorldHint"], json!(false));
    }

    /// A2 — is_known_tool recognizes all 9 tool names, rejects others.
    #[test]
    fn a2_is_known_tool() {
        for name in [
            "crw_scrape",
            "crw_crawl",
            "crw_check_crawl_status",
            "crw_map",
            "crw_search",
            "crw_parse_file",
            "crw_extract",
            "crw_check_extract_status",
            "crw_cancel_extract",
        ] {
            assert!(is_known_tool(name), "{name} should be known");
        }
        assert!(!is_known_tool("nonexistent"));
        assert!(!is_known_tool(""));
    }

    /// A3 — tools/list suppresses crw_search when no backend; includes it otherwise.
    #[test]
    fn a3_tools_list_conditional_search() {
        fn list(search_available: bool) -> Vec<String> {
            let req = JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(json!(1)),
                method: "tools/list".into(),
                params: json!({}),
            };
            let ProtocolResult::Response(resp) =
                handle_protocol_method("crw", "0", &req, false, search_available)
            else {
                panic!("expected response");
            };
            resp.result.unwrap()["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        }
        let with = list(true);
        assert!(with.contains(&"crw_search".to_string()));
        assert_eq!(with.len(), 9);
        let without = list(false);
        assert!(!without.contains(&"crw_search".to_string()));
        assert_eq!(without.len(), 8);
    }

    /// A4 — initialize advertises server usage `instructions` (the model-facing
    /// steering lever) and never leaks the search backend's identity anywhere in
    /// the advertised surface (tool descriptions + instructions).
    #[test]
    fn a4_initialize_advertises_instructions_no_backend_leak() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        let result = resp.result.unwrap();
        let instructions = result["instructions"]
            .as_str()
            .expect("initialize returns an instructions string");
        assert!(
            instructions.contains("crw_search"),
            "names the tools to prefer"
        );

        // instructions must agree with the advertised tool set: tools/list strips
        // crw_search when no backend is configured, so the guidance must not name it.
        let no_search = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let ProtocolResult::Response(resp2) =
            handle_protocol_method("crw", "0", &no_search, false, false)
        else {
            panic!("expected response");
        };
        let instructions2 = resp2.result.unwrap()["instructions"]
            .as_str()
            .expect("instructions string")
            .to_string();
        assert!(
            !instructions2.contains("crw_search"),
            "must not name crw_search when tools/list strips it"
        );

        // The advertised surface (descriptions + instructions) must never name the
        // search backend — locks the crw_search description SearXNG-leak fix.
        let advertised = format!("{instructions} {}", tool_definitions(false));
        assert!(
            !advertised.to_lowercase().contains("searxng"),
            "search backend identity must not leak into the advertised MCP surface"
        );
    }

    /// B13 — crw_search inlined scrape content (flat + grouped) is truncated.
    #[test]
    fn b13_search_inlined_content_is_bounded() {
        // Flat results with inlined markdown.
        let flat = json!({
            "success": true,
            "data": { "results": [
                { "url": "https://e.com/1", "markdown": long_md(DEFAULT_MAX_LENGTH + 100) },
                { "url": "https://e.com/2", "description": "no scrape content" }
            ]}
        });
        let out = apply_bounds("crw_search", &json!({}), flat);
        assert!(
            out["data"]["results"][0]["markdown"]
                .as_str()
                .unwrap()
                .contains("[truncated")
        );
        assert_eq!(out["data"]["results"][0]["truncated"], json!(true));
        assert!(out["data"]["results"][1].get("truncated").is_none());

        // Grouped results.
        let grouped = json!({
            "success": true,
            "data": { "results": {
                "web": [{ "url": "https://e.com/w", "html": long_md(DEFAULT_MAX_LENGTH + 100) }],
                "news": [{ "url": "https://e.com/n", "description": "short" }]
            }}
        });
        let out = apply_bounds("crw_search", &json!({}), grouped);
        assert_eq!(out["data"]["results"]["web"][0]["truncated"], json!(true));
        assert!(out["data"]["results"]["news"][0].get("truncated").is_none());
    }

    // --- Proxy-mode output contract (issue #391) ---

    /// The managed `/v1/search` body: results sit DIRECTLY in `data`, because the
    /// public REST contract hoists them one level out of the engine's envelope.
    fn managed_search_value() -> Value {
        json!({
            "success": true,
            "data": [search_result_item(1), search_result_item(2)]
        })
    }

    /// The managed `/v1/extract` body: the proxy resolves the job synchronously and
    /// returns results inline instead of the engine's `{id, status, urls}` accept.
    fn managed_extract_value() -> Value {
        json!({
            "success": true,
            "results": [{
                "url": "https://example.com/",
                "status": "completed",
                "data": { "title": "Example Domain" }
            }]
        })
    }

    /// P1 — proxy mode advertises no output contract at all, embedded mode keeps
    /// every one it has. A remote we do not author must not be promised a shape.
    #[test]
    fn p1_proxy_mode_advertises_no_output_schema() {
        let proxied = tool_definitions(true);
        for tool in proxied["tools"].as_array().expect("tools array") {
            assert!(
                tool.get("outputSchema").is_none(),
                "{} must not advertise an outputSchema in proxy mode",
                tool["name"]
            );
        }

        let embedded = tool_definitions(false);
        let declared: Vec<&str> = embedded["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter(|t| t.get("outputSchema").is_some())
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            declared,
            vec![
                "crw_extract",
                "crw_check_extract_status",
                "crw_cancel_extract",
                "crw_search"
            ],
            "embedded mode must keep the schemas it can honour"
        );
    }

    /// P2 — the deliberate asymmetry: no schema advertised in proxy mode, yet
    /// `structuredContent` is still emitted. Threading `proxy_mode` into
    /// `tool_output_schema` in the name of consistency would silently stop that,
    /// with every other test still green. This is the guard against that refactor.
    #[test]
    fn p2_proxy_mode_still_emits_structured_content_without_a_schema() {
        let proxied = tool_definitions(true);
        let search = tool_by_name(&proxied, "crw_search");
        assert!(search.get("outputSchema").is_none());

        let resp = tool_result_response(json!(1), "crw_search", Ok(managed_search_value()));
        let result = result_of(&resp);
        assert_eq!(result["structuredContent"], managed_search_value());
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().expect("text"))
                .expect("text block is the same JSON"),
            managed_search_value()
        );
    }

    /// P3 — why P1 is not cosmetic: neither managed body can satisfy the schema the
    /// engine declares, so advertising it to a proxy caller is a promise that hard-
    /// fails the whole call on every strict client. Asserts the SPECIFIC mismatch,
    /// not merely "some validation error" — a vaguer assertion would keep passing if
    /// the fixture drifted or the schema started rejecting an unrelated field.
    /// The managed synchronous extract body has no coverage anywhere else (the
    /// existing extract locks in `crw-server/tests/mcp.rs` all exercise the ENGINE's
    /// async accept envelope, which does satisfy the schema).
    #[test]
    fn p3_managed_bodies_cannot_satisfy_the_engine_schemas() {
        // `crw_search`: `data` is an array, the schema demands an object with `results`.
        let search_errors = schema_errors("crw_search", &managed_search_value());
        assert!(
            search_errors
                .iter()
                .any(|(path, msg)| path == "/data" && msg.contains("object")),
            "crw_search: expected `/data` to fail the object requirement, got {search_errors:?}"
        );

        // `crw_extract`: the engine's accept envelope requires `id`/`status`/`urls`,
        // none of which a synchronously-resolved managed body carries, and `results`
        // is rejected outright by `additionalProperties: false`.
        let extract_errors = schema_errors("crw_extract", &managed_extract_value());
        let joined = extract_errors
            .iter()
            .map(|(p, m)| format!("{p}: {m}"))
            .collect::<Vec<_>>()
            .join(" | ");
        for missing in ["id", "status", "urls"] {
            assert!(
                joined.contains(missing),
                "crw_extract: expected a complaint about the missing `{missing}`, got {joined}"
            );
        }
        assert!(
            joined.contains("results"),
            "crw_extract: expected `results` to be rejected by additionalProperties:false, \
             got {joined}"
        );

        // And therefore proxy mode must advertise neither.
        for tool in ["crw_search", "crw_extract"] {
            assert!(
                tool_by_name(&tool_definitions(true), tool)
                    .get("outputSchema")
                    .is_none(),
                "{tool}: proxy mode must not advertise a schema it cannot honour"
            );
        }
    }

    /// `(instance_path, message)` for every way `body` fails `tool`'s engine schema.
    fn schema_errors(tool: &str, body: &Value) -> Vec<(String, String)> {
        let schema = tool_output_schema(tool).expect("engine declares a schema");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let errors: Vec<(String, String)> = validator
            .iter_errors(body)
            .map(|e| (e.instance_path().to_string(), e.to_string()))
            .collect();
        assert!(
            !errors.is_empty(),
            "{tool}: the managed body was expected to FAIL the engine schema; if this starts \
             passing the surfaces have converged and the proxy strip can be revisited"
        );
        errors
    }

    /// P4 — the default bound also has to bite on the managed shape. It used to
    /// silently no-op there, so a `crw_search` with `scrapeOptions` shipped whole
    /// pages into the model's context.
    #[test]
    fn p4_search_inlined_content_is_bounded_on_the_managed_shape() {
        // Flat: results directly in `data`.
        let flat = json!({
            "success": true,
            "data": [
                { "url": "https://e.com/1", "markdown": long_md(DEFAULT_MAX_LENGTH + 100) },
                { "url": "https://e.com/2", "description": "no scrape content" }
            ]
        });
        let out = apply_bounds("crw_search", &json!({}), flat);
        assert!(
            out["data"][0]["markdown"]
                .as_str()
                .expect("markdown")
                .contains("[truncated")
        );
        assert_eq!(out["data"][0]["truncated"], json!(true));
        assert!(out["data"][1].get("truncated").is_none());

        // Grouped: `{web,news,images}` directly in `data`.
        let grouped = json!({
            "success": true,
            "data": {
                "web": [{ "url": "https://e.com/w", "html": long_md(DEFAULT_MAX_LENGTH + 100) }],
                "news": [{ "url": "https://e.com/n", "description": "short" }]
            }
        });
        let out = apply_bounds("crw_search", &json!({}), grouped);
        assert_eq!(out["data"]["web"][0]["truncated"], json!(true));
        assert!(out["data"]["news"][0].get("truncated").is_none());
    }

    // --- strip_credit_fields ---

    /// Every known placement of a credit key is stripped, at any depth, while
    /// sibling fields (incl. `tokensUsed`, which is real LLM telemetry and not
    /// SaaS credit bookkeeping) are left intact.
    #[test]
    fn strip_credit_fields_covers_all_known_placements() {
        // Mimics the union of credit-bearing shapes: bare ScrapeData
        // (embedded crw_scrape), an enveloped crawl page array, a v2
        // metadata block, and an extract-status envelope.
        let mut value = json!({
            "success": true,
            "creditsUsed": 4,
            "tokensUsed": 1200,
            "creditCost": 1,
            "data": [
                { "url": "https://e.com/1", "creditCost": 1, "markdown": "a" },
                { "url": "https://e.com/2", "creditCost": 0, "metadata": {
                    "creditsUsed": 1, "title": "t"
                } }
            ],
            "results": [
                { "url": "https://e.com/3", "status": "completed", "creditsUsed": 3,
                  "data": { "title": "nested" } }
            ]
        });
        strip_credit_fields(&mut value);

        let serialized = serde_json::to_string(&value).expect("serialize");
        assert!(
            !serialized.contains("creditCost"),
            "no creditCost anywhere: {serialized}"
        );
        assert!(
            !serialized.contains("creditsUsed"),
            "no creditsUsed anywhere: {serialized}"
        );
        assert_eq!(value["success"], json!(true));
        assert_eq!(value["tokensUsed"], json!(1200));
        assert_eq!(value["data"][0]["markdown"], json!("a"));
        assert_eq!(value["data"][1]["metadata"]["title"], json!("t"));
        assert_eq!(value["results"][0]["data"]["title"], json!("nested"));
    }

    /// Caller-shaped extraction output is never touched. `ScrapeData.json` and
    /// an extract `results[].data` are built from the caller's own
    /// `jsonSchema`, so a field they named `creditCost` is their scraped value
    /// — stripping it would be silent data loss in the one part of the response
    /// the engine does not author. Engine bookkeeping around them still goes.
    #[test]
    fn strip_credit_fields_preserves_caller_extraction_output() {
        // Someone extracting a pricing page with `{ creditCost, creditsUsed }`
        // in their schema: plausible, and indistinguishable by key name alone.
        let mut value = json!({
            "creditCost": 1,
            "json": { "plan": "pro", "creditCost": 19.99, "creditsUsed": 500 },
            "data": [
                { "url": "https://e.com/1", "creditCost": 1,
                  "json": { "creditCost": 4.5 } }
            ],
            "results": [
                { "url": "https://e.com/2", "creditsUsed": 2,
                  "data": { "creditCost": 7, "nested": { "creditsUsed": 9 } } }
            ]
        });
        strip_credit_fields(&mut value);

        // Caller payloads intact, at every depth.
        assert_eq!(value["json"]["creditCost"], json!(19.99));
        assert_eq!(value["json"]["creditsUsed"], json!(500));
        assert_eq!(value["data"][0]["json"]["creditCost"], json!(4.5));
        assert_eq!(value["results"][0]["data"]["creditCost"], json!(7));
        assert_eq!(
            value["results"][0]["data"]["nested"]["creditsUsed"],
            json!(9)
        );

        // Engine bookkeeping wrapping those payloads is still stripped.
        assert!(value.get("creditCost").is_none());
        assert!(value["data"][0].get("creditCost").is_none());
        assert!(value["results"][0].get("creditsUsed").is_none());
    }

    /// A body with no credit keys is returned byte-identical — the strip is a
    /// pure no-op, never a rewrite of unrelated content.
    #[test]
    fn strip_credit_fields_noop_without_credits() {
        let mut value = json!({
            "success": true,
            "tokensUsed": 7,
            "data": { "markdown": "hello", "metadata": { "title": "t" } }
        });
        let before = value.clone();
        strip_credit_fields(&mut value);
        assert_eq!(value, before);
    }

    /// The extract-status output schema must accept a response whose
    /// `creditsUsed` key was stripped: the self-hosted `hide_credits` option
    /// makes that key disappear, and `additionalProperties: false` means a
    /// stale `required` entry would hard-fail strict clients. Companion to the
    /// server-side test that asserts the stripped body validates.
    #[test]
    fn extract_status_schema_does_not_require_credits_used() {
        let schema = extract_status_output_schema();
        let required = schema["required"].as_array().expect("required array");
        assert!(
            !required.iter().any(|v| v == "creditsUsed"),
            "creditsUsed must stay schema-optional so hide_credits bodies validate"
        );
        assert!(required.iter().any(|v| v == "tokensUsed"));
        assert!(schema["properties"]["creditsUsed"].is_object());
    }

    // ============================================================
    // Expansion pass: constants, serde framing, per-tool schema
    // shape, protocol-method dispatch, and boundary coverage of the
    // private truncation/bounding/stripping helpers.
    // ============================================================

    // --- Constants ---

    #[test]
    fn const_protocol_version_is_2025_06_18() {
        assert_eq!(PROTOCOL_VERSION, "2025-06-18");
    }

    #[test]
    fn const_default_max_length_is_15000() {
        assert_eq!(DEFAULT_MAX_LENGTH, 15_000);
    }

    #[test]
    fn const_default_map_limit_is_100() {
        assert_eq!(DEFAULT_MAP_LIMIT, 100);
    }

    #[test]
    fn const_server_instructions_differ_with_and_without_search() {
        assert_ne!(SERVER_INSTRUCTIONS, SERVER_INSTRUCTIONS_NO_SEARCH);
        assert!(SERVER_INSTRUCTIONS.contains("crw_search"));
        assert!(!SERVER_INSTRUCTIONS_NO_SEARCH.contains("crw_search"));
    }

    #[test]
    fn server_instructions_fn_picks_the_matching_variant() {
        assert_eq!(server_instructions(true), SERVER_INSTRUCTIONS);
        assert_eq!(server_instructions(false), SERVER_INSTRUCTIONS_NO_SEARCH);
    }

    // --- JsonRpcRequest deserialization ---

    #[test]
    fn request_deserializes_numeric_id() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#).unwrap();
        assert_eq!(req.id, Some(json!(42)));
        assert_eq!(req.method, "ping");
    }

    #[test]
    fn request_deserializes_string_id() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc-123","method":"ping"}"#).unwrap();
        assert_eq!(req.id, Some(json!("abc-123")));
    }

    #[test]
    fn request_missing_id_is_none() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert_eq!(req.id, None);
    }

    #[test]
    fn request_explicit_null_id_collapses_to_none() {
        // serde_json's Option<Value> deserialization treats a JSON `null` the
        // same as an absent key (visit_none), so an explicit `"id": null`
        // is indistinguishable from a missing id at this layer. Downstream
        // code relies on this: `req.id.clone().unwrap_or(Value::Null)`
        // produces the same JSON-RPC null id either way.
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
        assert_eq!(req.id, None);
    }

    #[test]
    fn request_missing_params_defaults_to_null() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn request_params_object_is_preserved() {
        let req: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"crw_scrape"}}"#,
        )
        .unwrap();
        assert_eq!(req.params["name"], json!("crw_scrape"));
    }

    #[test]
    fn request_unknown_top_level_field_is_ignored_not_fatal() {
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","totallyUnknownField":true}"#,
        );
        assert!(result.is_ok(), "unrecognized fields must not be fatal");
    }

    #[test]
    fn request_missing_jsonrpc_field_errors() {
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(r#"{"id":1,"method":"ping"}"#);
        assert!(result.is_err(), "jsonrpc has no default, must be required");
    }

    #[test]
    fn request_missing_method_field_errors() {
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1}"#);
        assert!(result.is_err(), "method has no default, must be required");
    }

    #[test]
    fn request_malformed_json_syntax_does_not_panic() {
        let result: Result<JsonRpcRequest, _> = serde_json::from_str(r#"{"jsonrpc":"2.0","#);
        assert!(result.is_err());
    }

    #[test]
    fn request_batch_array_at_top_level_errors_cleanly() {
        // This crate's `JsonRpcRequest` does not model JSON-RPC batching; a
        // batch array must fail to deserialize as a single request rather
        // than silently misparsing or panicking.
        let result: Result<JsonRpcRequest, _> =
            serde_json::from_str(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#);
        assert!(result.is_err());
    }

    #[test]
    fn request_empty_body_errors_cleanly() {
        let result: Result<JsonRpcRequest, _> = serde_json::from_str("");
        assert!(result.is_err());
    }

    #[test]
    fn request_method_can_be_empty_string() {
        // Structurally legal (the empty string is still a String); semantic
        // rejection of an empty method happens in handle_protocol_method's
        // NotHandled fallthrough, not at deserialization.
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":""}"#).unwrap();
        assert_eq!(req.method, "");
    }

    #[test]
    fn request_unicode_and_emoji_in_params_round_trip() {
        let req: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"query":"café 😀 日本"}}"#,
        )
        .unwrap();
        assert_eq!(req.params["query"], json!("café 😀 日本"));
    }

    // --- JsonRpcResponse / JsonRpcError construction & serde ---

    #[test]
    fn response_success_serializes_without_error_key() {
        let resp = JsonRpcResponse::success(json!(1), json!({"ok": true}));
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v.get("error").is_none());
        assert_eq!(v["result"], json!({"ok": true}));
        assert_eq!(v["jsonrpc"], json!("2.0"));
        assert_eq!(v["id"], json!(1));
    }

    #[test]
    fn response_error_serializes_without_result_key() {
        let resp = JsonRpcResponse::error(json!("x"), -32601, "method not found".into());
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v.get("result").is_none());
        assert_eq!(v["error"]["code"], json!(-32601));
        assert_eq!(v["error"]["message"], json!("method not found"));
        assert_eq!(v["id"], json!("x"));
    }

    #[test]
    fn response_preserves_null_id() {
        let resp = JsonRpcResponse::success(Value::Null, json!({}));
        assert_eq!(resp.id, Value::Null);
    }

    #[test]
    fn response_error_code_is_i64_and_can_be_negative_or_large() {
        let resp = JsonRpcResponse::error(json!(1), i64::MIN, "boom".into());
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["error"]["code"], json!(i64::MIN));
    }

    // --- tool_definitions(): tool set shape ---

    #[test]
    fn tool_definitions_has_exactly_9_tools_regardless_of_proxy_mode() {
        assert_eq!(
            tool_definitions(false)["tools"].as_array().unwrap().len(),
            9
        );
        assert_eq!(tool_definitions(true)["tools"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn tool_definitions_names_are_unique() {
        let defs = tool_definitions(false);
        let names: Vec<&str> = defs["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate tool name in {names:?}"
        );
    }

    /// Every tool's `required` array in `inputSchema` lists only names that
    /// actually appear in `properties` — catches a typo'd required field
    /// that would make the schema permanently unsatisfiable.
    #[test]
    fn every_tool_required_field_exists_in_properties() {
        let defs = tool_definitions(false);
        for t in defs["tools"].as_array().unwrap() {
            let props = t["inputSchema"]["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} missing properties object", t["name"]));
            let required = t["inputSchema"]["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{} missing required array", t["name"]));
            for r in required {
                let name = r.as_str().unwrap();
                assert!(
                    props.contains_key(name),
                    "{}: required field {name} not declared in properties",
                    t["name"]
                );
            }
        }
    }

    macro_rules! required_fields_test {
        ($fn_name:ident, $tool:expr, $expected:expr) => {
            #[test]
            fn $fn_name() {
                let defs = tool_definitions(false);
                let tool = tool_by_name(&defs, $tool);
                let required: Vec<&str> = tool["inputSchema"]["required"]
                    .as_array()
                    .expect("required array")
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert_eq!(required, $expected, "{} required fields", $tool);
            }
        };
    }

    required_fields_test!(crw_scrape_required_is_url_only, "crw_scrape", vec!["url"]);
    required_fields_test!(crw_crawl_required_is_url_only, "crw_crawl", vec!["url"]);
    required_fields_test!(
        crw_check_crawl_status_required_is_id_only,
        "crw_check_crawl_status",
        vec!["id"]
    );
    required_fields_test!(crw_map_required_is_url_only, "crw_map", vec!["url"]);
    required_fields_test!(
        crw_extract_required_is_urls_only,
        "crw_extract",
        vec!["urls"]
    );
    required_fields_test!(
        crw_check_extract_status_required_is_id_only,
        "crw_check_extract_status",
        vec!["id"]
    );
    required_fields_test!(
        crw_cancel_extract_required_is_id_only,
        "crw_cancel_extract",
        vec!["id"]
    );
    required_fields_test!(
        crw_search_required_is_query_only,
        "crw_search",
        vec!["query"]
    );
    required_fields_test!(
        crw_parse_file_required_is_content_base64_only,
        "crw_parse_file",
        vec!["contentBase64"]
    );

    #[test]
    fn crw_scrape_formats_enum_has_the_four_documented_values() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let enum_vals = scrape["inputSchema"]["properties"]["formats"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(
            enum_vals,
            &vec![
                json!("markdown"),
                json!("html"),
                json!("links"),
                json!("images")
            ]
        );
    }

    #[test]
    fn crw_scrape_max_length_has_minimum_zero() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        assert_eq!(
            scrape["inputSchema"]["properties"]["maxLength"]["minimum"],
            json!(0)
        );
    }

    #[test]
    fn crw_crawl_json_schema_field_allows_additional_properties() {
        let defs = tool_definitions(false);
        let crawl = tool_by_name(&defs, "crw_crawl");
        assert_eq!(
            crawl["inputSchema"]["properties"]["jsonSchema"]["additionalProperties"],
            json!(true)
        );
    }

    #[test]
    fn crw_map_boolean_flags_are_typed_boolean() {
        let defs = tool_definitions(false);
        let map = tool_by_name(&defs, "crw_map");
        let props = &map["inputSchema"]["properties"];
        assert_eq!(props["useSitemap"]["type"], "boolean");
        assert_eq!(props["crawlFallback"]["type"], "boolean");
        assert_eq!(props["limit"]["type"], "integer");
        assert_eq!(props["limit"]["minimum"], json!(0));
    }

    #[test]
    fn crw_extract_optional_byok_fields_are_strings() {
        let defs = tool_definitions(false);
        let extract = tool_by_name(&defs, "crw_extract");
        let props = &extract["inputSchema"]["properties"];
        for key in ["llmApiKey", "llmProvider", "llmModel"] {
            assert_eq!(props[key]["type"], "string", "{key} must be a string");
        }
        assert_eq!(props["basis"]["type"], "boolean");
        assert_eq!(props["urls"]["items"]["type"], "string");
    }

    #[test]
    fn crw_search_tbs_enum_has_the_five_time_windows() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let enum_vals = search["inputSchema"]["properties"]["tbs"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(
            enum_vals,
            &vec![
                json!("qdr:h"),
                json!("qdr:d"),
                json!("qdr:w"),
                json!("qdr:m"),
                json!("qdr:y")
            ]
        );
    }

    #[test]
    fn crw_search_sources_enum_has_web_news_images() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let enum_vals = search["inputSchema"]["properties"]["sources"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(
            enum_vals,
            &vec![json!("web"), json!("news"), json!("images")]
        );
    }

    #[test]
    fn crw_search_scrape_options_formats_include_raw_html() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let formats = search["inputSchema"]["properties"]["scrapeOptions"]["properties"]["formats"]
            ["items"]["enum"]
            .as_array()
            .unwrap();
        assert!(formats.iter().any(|v| v == "rawHtml"));
        assert!(formats.iter().any(|v| v == "markdown"));
    }

    #[test]
    fn crw_parse_file_formats_enum_includes_json_and_summary() {
        let defs = tool_definitions(false);
        let parse = tool_by_name(&defs, "crw_parse_file");
        let enum_vals = parse["inputSchema"]["properties"]["formats"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert!(enum_vals.iter().any(|v| v == "json"));
        assert!(enum_vals.iter().any(|v| v == "summary"));
        assert!(enum_vals.iter().any(|v| v == "plainText"));
    }

    #[test]
    fn crw_parse_file_parsers_enum_is_pdf_only() {
        let defs = tool_definitions(false);
        let parse = tool_by_name(&defs, "crw_parse_file");
        let enum_vals = parse["inputSchema"]["properties"]["parsers"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(enum_vals, &vec![json!("pdf")]);
    }

    #[test]
    fn crw_parse_file_json_schema_field_is_free_form_object() {
        let defs = tool_definitions(false);
        let parse = tool_by_name(&defs, "crw_parse_file");
        assert_eq!(
            parse["inputSchema"]["properties"]["jsonSchema"]["additionalProperties"],
            json!(true)
        );
    }

    #[test]
    fn additional_properties_false_absent_from_extra_tools_input_schemas() {
        // schemas_do_not_set_additional_properties_false above only covers
        // scrape/crawl/map; extend the same guard to the remaining tools.
        let defs = tool_definitions(false);
        for name in [
            "crw_extract",
            "crw_check_extract_status",
            "crw_cancel_extract",
            "crw_search",
            "crw_parse_file",
        ] {
            let tool = tool_by_name(&defs, name);
            let ap = tool["inputSchema"].get("additionalProperties");
            assert!(
                ap.is_none() || ap.and_then(|v| v.as_bool()) != Some(false),
                "{name}: inputSchema must not set additionalProperties:false"
            );
        }
    }

    // --- Annotations across the remaining tools (A1 only covered 4) ---

    #[test]
    fn crw_map_and_check_crawl_status_annotations_are_read_only_idempotent() {
        let defs = tool_definitions(false);
        for name in [
            "crw_map",
            "crw_check_crawl_status",
            "crw_check_extract_status",
        ] {
            let t = tool_by_name(&defs, name);
            assert_eq!(t["annotations"]["readOnlyHint"], json!(true), "{name}");
            assert_eq!(t["annotations"]["idempotentHint"], json!(true), "{name}");
            assert_eq!(t["annotations"]["openWorldHint"], json!(true), "{name}");
        }
    }

    #[test]
    fn crw_search_annotations_are_read_only_idempotent_open_world() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        assert_eq!(search["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(search["annotations"]["idempotentHint"], json!(true));
        assert_eq!(search["annotations"]["openWorldHint"], json!(true));
        assert_eq!(search["annotations"]["destructiveHint"], json!(false));
    }

    // --- extract_accepted_output_schema / extract_status_output_schema, direct ---

    #[test]
    fn extract_accepted_schema_compiles_as_valid_json_schema() {
        let schema = extract_accepted_output_schema();
        assert!(jsonschema::validator_for(&schema).is_ok());
    }

    #[test]
    fn extract_status_schema_compiles_as_valid_json_schema() {
        let schema = extract_status_output_schema();
        assert!(jsonschema::validator_for(&schema).is_ok());
    }

    #[test]
    fn extract_accepted_schema_validates_a_real_accept_body() {
        let schema = extract_accepted_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({"success": true, "id": "job-1", "status": "processing", "urls": 3});
        assert!(validator.is_valid(&body));
    }

    #[test]
    fn extract_accepted_schema_rejects_unknown_status_value() {
        let schema = extract_accepted_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({"success": true, "id": "job-1", "status": "done", "urls": 3});
        assert!(!validator.is_valid(&body));
    }

    #[test]
    fn extract_accepted_schema_rejects_extra_field() {
        let schema = extract_accepted_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({
            "success": true, "id": "job-1", "status": "processing", "urls": 3,
            "unexpectedField": "nope"
        });
        assert!(!validator.is_valid(&body));
    }

    #[test]
    fn extract_status_schema_validates_minimal_result_item() {
        let schema = extract_status_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({
            "success": true, "id": "job-1", "status": "completed",
            "results": [{"url": "https://e.com", "status": "completed"}],
            "expiresAt": "2026-01-01T00:00:00Z", "tokensUsed": 0
        });
        assert!(validator.is_valid(&body));
    }

    #[test]
    fn extract_status_schema_rejects_result_item_missing_url() {
        let schema = extract_status_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({
            "success": true, "id": "job-1", "status": "completed",
            "results": [{"status": "completed"}],
            "expiresAt": "2026-01-01T00:00:00Z", "tokensUsed": 0
        });
        assert!(!validator.is_valid(&body));
    }

    #[test]
    fn extract_status_schema_accepts_every_declared_status_enum_value() {
        let schema = extract_status_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for status in [
            "processing",
            "cancelling",
            "completed",
            "failed",
            "cancelled",
        ] {
            let body = json!({
                "success": true, "id": "job-1", "status": status,
                "results": [], "expiresAt": "2026-01-01T00:00:00Z", "tokensUsed": 0
            });
            assert!(validator.is_valid(&body), "status {status} should validate");
        }
    }

    #[test]
    fn extract_status_schema_rejects_status_outside_enum() {
        let schema = extract_status_output_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let body = json!({
            "success": true, "id": "job-1", "status": "queued",
            "results": [], "expiresAt": "2026-01-01T00:00:00Z", "tokensUsed": 0
        });
        assert!(!validator.is_valid(&body));
    }

    #[test]
    fn crw_extract_and_check_status_share_the_same_output_schema_shape() {
        // Both crw_check_extract_status and crw_cancel_extract advertise
        // extract_status_output_schema(); they must be byte-identical.
        let defs = tool_definitions(false);
        let check = tool_by_name(&defs, "crw_check_extract_status");
        let cancel = tool_by_name(&defs, "crw_cancel_extract");
        assert_eq!(check["outputSchema"], cancel["outputSchema"]);
    }

    // --- tool_output_schema ---

    #[test]
    fn tool_output_schema_present_for_the_four_schema_bearing_tools() {
        for name in [
            "crw_extract",
            "crw_check_extract_status",
            "crw_cancel_extract",
            "crw_search",
        ] {
            assert!(
                tool_output_schema(name).is_some(),
                "{name} should have a schema"
            );
        }
    }

    #[test]
    fn tool_output_schema_absent_for_schema_free_tools() {
        for name in [
            "crw_scrape",
            "crw_crawl",
            "crw_check_crawl_status",
            "crw_map",
            "crw_parse_file",
        ] {
            assert!(
                tool_output_schema(name).is_none(),
                "{name} should have no schema"
            );
        }
    }

    #[test]
    fn tool_output_schema_unknown_tool_name_is_none() {
        assert!(tool_output_schema("crw_does_not_exist").is_none());
        assert!(tool_output_schema("").is_none());
    }

    // --- is_known_tool edge cases ---

    #[test]
    fn is_known_tool_is_case_sensitive() {
        assert!(is_known_tool("crw_scrape"));
        assert!(!is_known_tool("CRW_SCRAPE"));
        assert!(!is_known_tool("Crw_Scrape"));
    }

    #[test]
    fn is_known_tool_rejects_near_miss_names() {
        assert!(!is_known_tool("crw-scrape"));
        assert!(!is_known_tool("crw_scrape "));
        assert!(!is_known_tool(" crw_scrape"));
        assert!(!is_known_tool("crw_scrape\n"));
        assert!(!is_known_tool("crw_scrap"));
    }

    #[test]
    fn is_known_tool_rejects_unicode_lookalike() {
        assert!(!is_known_tool("crw_scräpe"));
    }

    // --- handle_protocol_method ---

    #[test]
    fn proto_wrong_jsonrpc_version_yields_dash32600_and_echoes_id() {
        let req = JsonRpcRequest {
            jsonrpc: "1.0".into(),
            id: Some(json!("req-9")),
            method: "ping".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        assert_eq!(resp.id, json!("req-9"));
        let err = resp.error.expect("error present");
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "invalid jsonrpc version");
        assert!(resp.result.is_none());
    }

    #[test]
    fn proto_wrong_jsonrpc_version_missing_id_defaults_to_null() {
        let req = JsonRpcRequest {
            jsonrpc: "".into(),
            id: None,
            method: "ping".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        assert_eq!(resp.id, Value::Null);
    }

    #[test]
    fn proto_notifications_initialized_is_a_notification() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: Value::Null,
        };
        assert!(matches!(
            handle_protocol_method("crw", "0", &req, false, true),
            ProtocolResult::Notification
        ));
    }

    #[test]
    fn proto_notifications_cancelled_is_a_notification() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/cancelled".into(),
            params: Value::Null,
        };
        assert!(matches!(
            handle_protocol_method("crw", "0", &req, false, true),
            ProtocolResult::Notification
        ));
    }

    #[test]
    fn proto_ping_returns_empty_object_result() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(7)),
            method: "ping".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        assert_eq!(resp.id, json!(7));
        assert_eq!(resp.result, Some(json!({})));
    }

    #[test]
    fn proto_unknown_method_is_not_handled() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "resources/list".into(),
            params: Value::Null,
        };
        assert!(matches!(
            handle_protocol_method("crw", "0", &req, false, true),
            ProtocolResult::NotHandled
        ));
    }

    #[test]
    fn proto_empty_method_string_is_not_handled() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "".into(),
            params: Value::Null,
        };
        assert!(matches!(
            handle_protocol_method("crw", "0", &req, false, true),
            ProtocolResult::NotHandled
        ));
    }

    #[test]
    fn proto_initialize_echoes_server_name_and_version() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) =
            handle_protocol_method("crw-test-server", "9.9.9", &req, false, true)
        else {
            panic!("expected response");
        };
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], json!("crw-test-server"));
        assert_eq!(result["serverInfo"]["version"], json!("9.9.9"));
        assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(false));
    }

    #[test]
    fn proto_initialize_missing_id_defaults_to_null() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "initialize".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        assert_eq!(resp.id, Value::Null);
    }

    #[test]
    fn proto_tools_list_proxy_mode_strips_output_schema_end_to_end() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, true, true)
        else {
            panic!("expected response");
        };
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        for t in &tools {
            assert!(
                t.get("outputSchema").is_none(),
                "{} kept a schema in proxy mode",
                t["name"]
            );
        }
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn proto_tools_list_missing_id_defaults_to_null() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "tools/list".into(),
            params: Value::Null,
        };
        let ProtocolResult::Response(resp) = handle_protocol_method("crw", "0", &req, false, true)
        else {
            panic!("expected response");
        };
        assert_eq!(resp.id, Value::Null);
    }

    // --- tool_result_response: id handling across id shapes ---

    #[test]
    fn tool_result_response_preserves_string_id() {
        let resp = tool_result_response(json!("call-42"), "crw_scrape", Ok(json!({"x": 1})));
        assert_eq!(resp.id, json!("call-42"));
    }

    #[test]
    fn tool_result_response_preserves_null_id() {
        let resp = tool_result_response(Value::Null, "crw_scrape", Ok(json!({"x": 1})));
        assert_eq!(resp.id, Value::Null);
    }

    #[test]
    fn tool_result_response_err_path_preserves_id_too() {
        let resp = tool_result_response(json!(99), "crw_scrape", Err("nope".into()));
        assert_eq!(resp.id, json!(99));
    }

    #[test]
    fn tool_result_response_unknown_tool_name_gets_no_structured_content() {
        // tool_output_schema looks the name up; an unrecognized name simply
        // has no schema, same as any other schema-free tool.
        let resp = tool_result_response(json!(1), "crw_totally_made_up", Ok(json!({"a": 1})));
        let result = resp.result.unwrap();
        assert!(result.get("structuredContent").is_none());
    }

    #[test]
    fn tool_result_response_empty_object_ok_value_still_gets_structured_content() {
        // crw_search has a schema; an empty object is still an object, so
        // structuredContent is attached even though it is empty.
        let resp = tool_result_response(json!(1), "crw_search", Ok(json!({})));
        let result = resp.result.unwrap();
        assert_eq!(result["structuredContent"], json!({}));
    }

    #[test]
    fn tool_result_response_err_message_with_unicode_is_preserved_verbatim() {
        let resp = tool_result_response(json!(1), "crw_scrape", Err("caf\u{e9} \u{1f600}".into()));
        let result = resp.result.unwrap();
        assert_eq!(result["content"][0]["text"], json!("café 😀"));
    }

    // --- resolve_bound (private) ---

    #[test]
    fn resolve_bound_absent_key_returns_default() {
        assert_eq!(resolve_bound(&json!({}), "maxLength", 15_000), Some(15_000));
    }

    #[test]
    fn resolve_bound_explicit_zero_returns_none() {
        assert_eq!(
            resolve_bound(&json!({"maxLength": 0}), "maxLength", 15_000),
            None
        );
    }

    #[test]
    fn resolve_bound_positive_value_is_honored() {
        assert_eq!(resolve_bound(&json!({"limit": 25}), "limit", 100), Some(25));
    }

    #[test]
    fn resolve_bound_non_numeric_value_falls_back_to_default() {
        // as_u64() on a non-number yields None, which resolve_bound treats
        // the same as an absent key.
        assert_eq!(
            resolve_bound(&json!({"limit": "not a number"}), "limit", 100),
            Some(100)
        );
    }

    #[test]
    fn resolve_bound_negative_value_falls_back_to_default() {
        // as_u64() on a negative i64 returns None.
        assert_eq!(
            resolve_bound(&json!({"limit": -5}), "limit", 100),
            Some(100)
        );
    }

    #[test]
    fn resolve_bound_args_not_an_object_falls_back_to_default() {
        assert_eq!(resolve_bound(&json!([1, 2, 3]), "limit", 100), Some(100));
        assert_eq!(resolve_bound(&Value::Null, "limit", 100), Some(100));
    }

    #[test]
    fn resolve_bound_very_large_value_is_preserved() {
        assert_eq!(
            resolve_bound(&json!({"limit": u64::MAX}), "limit", 100),
            Some(u64::MAX as usize)
        );
    }

    // --- truncate_to_chars (private) ---

    #[test]
    fn truncate_to_chars_returns_none_when_under_the_cap() {
        assert_eq!(truncate_to_chars("short", 100), None);
    }

    #[test]
    fn truncate_to_chars_returns_none_when_exactly_at_the_cap() {
        let s = "x".repeat(10);
        assert_eq!(truncate_to_chars(&s, 10), None);
    }

    #[test]
    fn truncate_to_chars_truncates_when_one_over_the_cap() {
        let s = "x".repeat(11);
        let out = truncate_to_chars(&s, 10).expect("should truncate");
        assert!(out.starts_with(&"x".repeat(10)));
        assert!(out.contains("[truncated by crw-mcp maxLength]"));
    }

    #[test]
    fn truncate_to_chars_empty_string_never_truncates() {
        assert_eq!(truncate_to_chars("", 0), None);
        assert_eq!(truncate_to_chars("", 10), None);
    }

    #[test]
    fn truncate_to_chars_zero_cap_on_nonempty_string_truncates_to_nothing() {
        let out = truncate_to_chars("hello", 0).expect("should truncate");
        assert!(
            out.starts_with('\n'),
            "0-char cap keeps no content before the marker"
        );
    }

    #[test]
    fn truncate_to_chars_is_char_boundary_safe_with_multibyte_content() {
        let s = "é".repeat(20); // each é is 2 UTF-8 bytes
        let out = truncate_to_chars(&s, 5).expect("should truncate");
        assert!(out.starts_with(&"é".repeat(5)));
    }

    #[test]
    fn truncate_to_chars_handles_emoji_and_combining_marks_without_panicking() {
        let s = "👨‍👩‍👧‍👦".repeat(50); // family emoji, ZWJ sequences
        // Must not panic regardless of where the char cut lands.
        let _ = truncate_to_chars(&s, 3);
        let _ = truncate_to_chars(&s, 1);
        let _ = truncate_to_chars(&s, 0);
    }

    #[test]
    fn truncate_to_chars_very_long_string_truncates_correctly() {
        let s = "a".repeat(1_000_000);
        let out = truncate_to_chars(&s, 15_000).expect("should truncate");
        let prefix_len = out.find('\n').unwrap();
        assert_eq!(prefix_len, 15_000);
    }

    // --- truncate_scrape_obj (private) ---

    #[test]
    fn truncate_scrape_obj_only_touches_known_fields() {
        let mut v = json!({
            "markdown": "x".repeat(20),
            "unrelatedLongField": "y".repeat(20),
        });
        truncate_scrape_obj(&mut v, 5);
        assert!(v["markdown"].as_str().unwrap().contains("[truncated"));
        assert_eq!(v["unrelatedLongField"], json!("y".repeat(20)));
    }

    #[test]
    fn truncate_scrape_obj_truncates_multiple_fields_independently() {
        let mut v = json!({
            "markdown": "m".repeat(20),
            "html": "h".repeat(20),
            "rawHtml": "r".repeat(3),
            "plainText": "p".repeat(20),
            "summary": "s".repeat(20),
        });
        truncate_scrape_obj(&mut v, 5);
        assert!(v["markdown"].as_str().unwrap().contains("[truncated"));
        assert!(v["html"].as_str().unwrap().contains("[truncated"));
        assert!(
            !v["rawHtml"].as_str().unwrap().contains("[truncated"),
            "under cap, untouched"
        );
        assert!(v["plainText"].as_str().unwrap().contains("[truncated"));
        assert!(v["summary"].as_str().unwrap().contains("[truncated"));
        assert_eq!(v["truncated"], json!(true));
    }

    #[test]
    fn truncate_scrape_obj_non_string_field_is_skipped_without_panic() {
        let mut v = json!({ "markdown": 12345, "html": null, "rawHtml": ["not", "a", "string"] });
        truncate_scrape_obj(&mut v, 1);
        assert_eq!(v["markdown"], json!(12345));
        assert_eq!(v["html"], Value::Null);
        assert!(v.get("truncated").is_none());
    }

    #[test]
    fn truncate_scrape_obj_on_non_object_value_is_a_no_op() {
        let mut v = json!(["not", "an", "object"]);
        let before = v.clone();
        truncate_scrape_obj(&mut v, 5);
        assert_eq!(v, before);
    }

    #[test]
    fn truncate_scrape_obj_missing_fields_do_not_add_truncated_flag() {
        let mut v = json!({ "url": "https://e.com" });
        truncate_scrape_obj(&mut v, 5);
        assert!(v.get("truncated").is_none());
    }

    // --- scrape_target_mut (private) ---

    #[test]
    fn scrape_target_mut_returns_data_when_data_is_an_object() {
        let mut v = json!({ "success": true, "data": { "markdown": "hi" } });
        let target = scrape_target_mut(&mut v).expect("target");
        assert_eq!(target["markdown"], json!("hi"));
    }

    #[test]
    fn scrape_target_mut_returns_self_when_bare_object_with_no_data() {
        let mut v = json!({ "markdown": "hi" });
        let target = scrape_target_mut(&mut v).expect("target");
        assert_eq!(target["markdown"], json!("hi"));
    }

    #[test]
    fn scrape_target_mut_falls_back_to_self_when_data_is_not_an_object() {
        // `data` present but not an object (e.g. an array) does not satisfy
        // the `is_object()` guard, so the whole value (still an object) is
        // used as the target instead.
        let mut v = json!({ "data": [1, 2, 3] });
        let target = scrape_target_mut(&mut v).expect("target");
        assert_eq!(target["data"], json!([1, 2, 3]));
    }

    #[test]
    fn scrape_target_mut_none_for_non_object_root() {
        let mut arr = json!([1, 2, 3]);
        assert!(scrape_target_mut(&mut arr).is_none());
        let mut s = json!("just a string");
        assert!(scrape_target_mut(&mut s).is_none());
        let mut n = Value::Null;
        assert!(scrape_target_mut(&mut n).is_none());
    }

    // --- bound_map_links (private) ---

    #[test]
    fn bound_map_links_exactly_at_limit_is_untouched() {
        let links: Vec<Value> = (0..100).map(|i| json!(format!("u{i}"))).collect();
        let mut v = json!({ "links": links });
        bound_map_links(&mut v, 100);
        assert_eq!(v["links"].as_array().unwrap().len(), 100);
        assert!(v.get("truncated").is_none());
    }

    #[test]
    fn bound_map_links_one_over_limit_truncates() {
        let links: Vec<Value> = (0..101).map(|i| json!(format!("u{i}"))).collect();
        let mut v = json!({ "links": links });
        bound_map_links(&mut v, 100);
        assert_eq!(v["links"].as_array().unwrap().len(), 100);
        assert_eq!(v["totalDiscovered"], json!(101));
        assert_eq!(v["truncated"], json!(true));
    }

    #[test]
    fn bound_map_links_no_links_or_sitemaps_key_is_a_no_op() {
        let mut v = json!({ "success": true });
        bound_map_links(&mut v, 10);
        assert_eq!(v, json!({ "success": true }));
    }

    #[test]
    fn bound_map_links_on_non_object_value_is_a_no_op() {
        let mut v = json!([1, 2, 3]);
        let before = v.clone();
        bound_map_links(&mut v, 10);
        assert_eq!(v, before);
    }

    #[test]
    fn bound_map_links_proxy_envelope_at_data_is_bound_independently() {
        let links: Vec<Value> = (0..5).map(|i| json!(format!("u{i}"))).collect();
        let mut v = json!({ "success": true, "data": { "links": links } });
        bound_map_links(&mut v, 2);
        assert_eq!(v["data"]["links"].as_array().unwrap().len(), 2);
        assert_eq!(v["data"]["totalDiscovered"], json!(5));
        // The top-level object must not gain the markers meant for `data`.
        assert!(v.get("truncated").is_none());
    }

    // --- bound_search_results (private) ---

    #[test]
    fn bound_search_results_missing_data_key_is_a_no_op() {
        let mut v = json!({ "success": true });
        bound_search_results(&mut v, 5);
        assert_eq!(v, json!({ "success": true }));
    }

    #[test]
    fn bound_search_results_data_neither_array_nor_object_with_results_is_a_no_op() {
        let mut v = json!({ "data": "unexpected string shape" });
        bound_search_results(&mut v, 5);
        assert_eq!(v["data"], json!("unexpected string shape"));
    }

    #[test]
    fn bound_search_results_empty_results_array_is_a_no_op() {
        let mut v = json!({ "data": { "results": [] } });
        bound_search_results(&mut v, 5);
        assert_eq!(v["data"]["results"], json!([]));
    }

    #[test]
    fn bound_search_results_grouped_with_an_empty_group_does_not_panic() {
        let mut v = json!({ "data": { "results": { "web": [], "news": [] } } });
        bound_search_results(&mut v, 5);
        assert_eq!(v["data"]["results"]["web"], json!([]));
    }

    // --- apply_bounds: remaining tool coverage ---

    #[test]
    fn apply_bounds_crw_parse_file_shares_the_scrape_truncation_path() {
        let value = json!({ "markdown": long_md(DEFAULT_MAX_LENGTH + 50) });
        let out = apply_bounds("crw_parse_file", &json!({}), value);
        assert!(out["markdown"].as_str().unwrap().contains("[truncated"));
        assert_eq!(out["truncated"], json!(true));
    }

    #[test]
    fn apply_bounds_crw_extract_is_untouched_passthrough() {
        let value = json!({ "id": "job-1", "status": "processing", "urls": 3 });
        let out = apply_bounds("crw_extract", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_crw_cancel_extract_is_untouched_passthrough() {
        let value = json!({ "id": "job-1", "status": "cancelling" });
        let out = apply_bounds("crw_cancel_extract", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_crw_check_extract_status_is_untouched_passthrough() {
        let value = json!({ "id": "job-1", "status": "completed", "results": [] });
        let out = apply_bounds("crw_check_extract_status", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_unrecognized_tool_name_is_untouched_passthrough() {
        let value = json!({ "anything": true });
        let out = apply_bounds("not_a_real_tool", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_scrape_at_exactly_the_default_cap_is_untouched() {
        let value = json!({ "markdown": long_md(DEFAULT_MAX_LENGTH) });
        let out = apply_bounds("crw_scrape", &json!({}), value);
        assert!(out.get("truncated").is_none());
    }

    #[test]
    fn apply_bounds_map_default_limit_zero_link_list_is_a_no_op() {
        let value = json!({ "links": [] });
        let out = apply_bounds("crw_map", &json!({}), value);
        assert!(out.get("truncated").is_none());
    }

    #[test]
    fn apply_bounds_check_crawl_status_missing_data_field_is_a_no_op() {
        let value = json!({ "status": "processing" });
        let out = apply_bounds("crw_check_crawl_status", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_check_crawl_status_max_length_zero_is_unbounded() {
        let value = json!({
            "data": [{ "markdown": long_md(DEFAULT_MAX_LENGTH * 2), "url": "https://e.com" }]
        });
        let out = apply_bounds("crw_check_crawl_status", &json!({ "maxLength": 0 }), value);
        assert!(out["data"][0].get("truncated").is_none());
    }

    // --- strip_mcp_only_args: exhaustive per tool ---

    #[test]
    fn strip_mcp_only_args_crw_parse_file_strips_max_length() {
        let out = strip_mcp_only_args(
            "crw_parse_file",
            json!({ "contentBase64": "abc", "maxLength": 10 }),
        );
        assert!(out.get("maxLength").is_none());
        assert_eq!(out["contentBase64"], json!("abc"));
    }

    #[test]
    fn strip_mcp_only_args_crw_check_crawl_status_strips_max_length() {
        let out = strip_mcp_only_args(
            "crw_check_crawl_status",
            json!({ "id": "j1", "maxLength": 10 }),
        );
        assert!(out.get("maxLength").is_none());
    }

    #[test]
    fn strip_mcp_only_args_crw_crawl_is_passthrough_unchanged() {
        let value = json!({ "url": "u", "maxDepth": 2 });
        let out = strip_mcp_only_args("crw_crawl", value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn strip_mcp_only_args_crw_extract_is_passthrough_unchanged() {
        let value = json!({ "urls": ["u"], "prompt": "p" });
        let out = strip_mcp_only_args("crw_extract", value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn strip_mcp_only_args_crw_check_extract_status_is_passthrough_unchanged() {
        let value = json!({ "id": "j1" });
        let out = strip_mcp_only_args("crw_check_extract_status", value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn strip_mcp_only_args_crw_cancel_extract_is_passthrough_unchanged() {
        let value = json!({ "id": "j1" });
        let out = strip_mcp_only_args("crw_cancel_extract", value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn strip_mcp_only_args_crw_map_does_not_strip_max_length_either() {
        // crw_map is not in the stripped-tool match arm at all: it has no
        // maxLength knob to strip in the first place, but if a caller sends
        // one anyway it is not this function's job to remove it.
        let out = strip_mcp_only_args("crw_map", json!({ "url": "u", "maxLength": 10 }));
        assert_eq!(out["maxLength"], json!(10));
    }

    #[test]
    fn strip_mcp_only_args_on_non_object_args_is_a_no_op() {
        let arr = json!([1, 2, 3]);
        assert_eq!(strip_mcp_only_args("crw_scrape", arr.clone()), arr);
        assert_eq!(strip_mcp_only_args("crw_scrape", Value::Null), Value::Null);
    }

    #[test]
    fn strip_mcp_only_args_absent_max_length_is_a_no_op() {
        let value = json!({ "url": "u" });
        let out = strip_mcp_only_args("crw_scrape", value.clone());
        assert_eq!(out, value);
    }

    // --- strip_credit_fields_inner (private): edge cases ---

    #[test]
    fn strip_credit_fields_inner_on_scalar_root_does_not_panic() {
        let mut v = json!(42);
        strip_credit_fields_inner(&mut v, false);
        assert_eq!(v, json!(42));

        let mut s = json!("just text");
        strip_credit_fields_inner(&mut s, false);
        assert_eq!(s, json!("just text"));

        let mut n = Value::Null;
        strip_credit_fields_inner(&mut n, false);
        assert_eq!(n, Value::Null);

        let mut b = json!(true);
        strip_credit_fields_inner(&mut b, false);
        assert_eq!(b, json!(true));
    }

    #[test]
    fn strip_credit_fields_handles_nested_arrays_of_arrays() {
        let mut v = json!({
            "data": [
                [
                    { "url": "a", "creditCost": 1 },
                    { "url": "b", "creditCost": 2 }
                ]
            ]
        });
        strip_credit_fields(&mut v);
        assert!(v["data"][0][0].get("creditCost").is_none());
        assert!(v["data"][0][1].get("creditCost").is_none());
        assert_eq!(v["data"][0][0]["url"], json!("a"));
    }

    #[test]
    fn strip_credit_fields_on_empty_object_is_a_no_op() {
        let mut v = json!({});
        strip_credit_fields(&mut v);
        assert_eq!(v, json!({}));
    }

    #[test]
    fn strip_credit_fields_on_empty_array_is_a_no_op() {
        let mut v = json!([]);
        strip_credit_fields(&mut v);
        assert_eq!(v, json!([]));
    }

    #[test]
    fn strip_credit_fields_top_level_array_of_objects() {
        let mut v = json!([
            { "url": "a", "creditsUsed": 1 },
            { "url": "b", "creditsUsed": 2 }
        ]);
        strip_credit_fields(&mut v);
        assert!(v[0].get("creditsUsed").is_none());
        assert!(v[1].get("creditsUsed").is_none());
    }

    #[test]
    fn strip_credit_fields_results_data_string_value_is_left_alone() {
        // `data` under `results[]` is treated as caller-shaped and skipped
        // entirely (not just its creditCost key) — including when it is not
        // even an object.
        let mut v = json!({
            "results": [{ "url": "a", "data": "raw string value with creditCost inside" }]
        });
        strip_credit_fields(&mut v);
        assert_eq!(
            v["results"][0]["data"],
            json!("raw string value with creditCost inside")
        );
    }

    // --- More per-tool schema field types ---

    #[test]
    fn crw_crawl_max_depth_and_max_pages_are_integers() {
        let defs = tool_definitions(false);
        let crawl = tool_by_name(&defs, "crw_crawl");
        let props = &crawl["inputSchema"]["properties"];
        assert_eq!(props["maxDepth"]["type"], "integer");
        assert_eq!(props["maxPages"]["type"], "integer");
    }

    #[test]
    fn crw_extract_schema_property_is_object_type() {
        let defs = tool_definitions(false);
        let extract = tool_by_name(&defs, "crw_extract");
        assert_eq!(
            extract["inputSchema"]["properties"]["schema"]["type"],
            "object"
        );
        assert_eq!(
            extract["inputSchema"]["properties"]["prompt"]["type"],
            "string"
        );
    }

    #[test]
    fn crw_check_crawl_status_max_length_has_minimum_zero() {
        let defs = tool_definitions(false);
        let status = tool_by_name(&defs, "crw_check_crawl_status");
        assert_eq!(
            status["inputSchema"]["properties"]["maxLength"]["minimum"],
            json!(0)
        );
    }

    #[test]
    fn crw_search_scrape_options_nested_field_types() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let opts = &search["inputSchema"]["properties"]["scrapeOptions"]["properties"];
        assert_eq!(opts["onlyMainContent"]["type"], "boolean");
        assert_eq!(opts["timeout"]["type"], "integer");
    }

    #[test]
    fn crw_parse_file_max_length_has_minimum_zero() {
        let defs = tool_definitions(false);
        let parse = tool_by_name(&defs, "crw_parse_file");
        assert_eq!(
            parse["inputSchema"]["properties"]["maxLength"]["minimum"],
            json!(0)
        );
    }

    #[test]
    fn crw_map_url_property_is_a_string() {
        let defs = tool_definitions(false);
        let map = tool_by_name(&defs, "crw_map");
        assert_eq!(map["inputSchema"]["properties"]["url"]["type"], "string");
    }

    #[test]
    fn crw_scrape_include_and_exclude_tags_are_string_arrays() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        let props = &scrape["inputSchema"]["properties"];
        assert_eq!(props["includeTags"]["type"], "array");
        assert_eq!(props["includeTags"]["items"]["type"], "string");
        assert_eq!(props["excludeTags"]["type"], "array");
        assert_eq!(props["excludeTags"]["items"]["type"], "string");
    }

    #[test]
    fn crw_scrape_only_main_content_is_boolean() {
        let defs = tool_definitions(false);
        let scrape = tool_by_name(&defs, "crw_scrape");
        assert_eq!(
            scrape["inputSchema"]["properties"]["onlyMainContent"]["type"],
            "boolean"
        );
    }

    // --- Determinism / purity of tool_definitions ---

    #[test]
    fn tool_definitions_false_is_deterministic_across_calls() {
        assert_eq!(tool_definitions(false), tool_definitions(false));
    }

    #[test]
    fn tool_definitions_true_is_deterministic_across_calls() {
        assert_eq!(tool_definitions(true), tool_definitions(true));
    }

    // --- is_known_tool: one assertion per real tool name (isolated failure attribution) ---

    macro_rules! known_tool_test {
        ($fn_name:ident, $name:expr) => {
            #[test]
            fn $fn_name() {
                assert!(is_known_tool($name));
            }
        };
    }

    known_tool_test!(known_tool_crw_scrape, "crw_scrape");
    known_tool_test!(known_tool_crw_crawl, "crw_crawl");
    known_tool_test!(known_tool_crw_check_crawl_status, "crw_check_crawl_status");
    known_tool_test!(known_tool_crw_map, "crw_map");
    known_tool_test!(known_tool_crw_search, "crw_search");
    known_tool_test!(known_tool_crw_parse_file, "crw_parse_file");
    known_tool_test!(known_tool_crw_extract, "crw_extract");
    known_tool_test!(
        known_tool_crw_check_extract_status,
        "crw_check_extract_status"
    );
    known_tool_test!(known_tool_crw_cancel_extract, "crw_cancel_extract");

    // --- apply_bounds: additional boundary coverage via the public entry point ---

    #[test]
    fn apply_bounds_map_exactly_at_default_limit_is_untouched() {
        let links: Vec<Value> = (0..DEFAULT_MAP_LIMIT)
            .map(|i| json!(format!("u{i}")))
            .collect();
        let value = json!({ "links": links });
        let out = apply_bounds("crw_map", &json!({}), value);
        assert_eq!(out["links"].as_array().unwrap().len(), DEFAULT_MAP_LIMIT);
        assert!(out.get("truncated").is_none());
    }

    #[test]
    fn apply_bounds_search_max_length_zero_is_unbounded() {
        let big = long_md(DEFAULT_MAX_LENGTH * 2);
        let value = json!({ "data": { "results": [{ "url": "u", "markdown": big.clone() }] } });
        let out = apply_bounds("crw_search", &json!({ "maxLength": 0 }), value);
        assert_eq!(
            out["data"]["results"][0]["markdown"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            big.chars().count()
        );
        assert!(out["data"]["results"][0].get("truncated").is_none());
    }

    #[test]
    fn apply_bounds_scrape_negative_max_length_falls_back_to_default() {
        // A negative maxLength cannot be represented as u64, so resolve_bound
        // treats it as absent and the default cap still applies.
        let value = json!({ "markdown": long_md(DEFAULT_MAX_LENGTH + 500) });
        let out = apply_bounds("crw_scrape", &json!({ "maxLength": -1 }), value);
        assert!(out["markdown"].as_str().unwrap().contains("[truncated"));
    }

    #[test]
    fn apply_bounds_check_crawl_status_empty_pages_array_is_untouched() {
        let value = json!({ "status": "completed", "data": [] });
        let out = apply_bounds("crw_check_crawl_status", &json!({}), value.clone());
        assert_eq!(out, value);
    }

    #[test]
    fn apply_bounds_map_links_and_sitemaps_both_over_limit_via_public_entry_point() {
        let links: Vec<Value> = (0..150).map(|i| json!(format!("l{i}"))).collect();
        let sitemaps: Vec<Value> = (0..150).map(|i| json!(format!("s{i}"))).collect();
        let value = json!({ "links": links, "sitemaps": sitemaps });
        let out = apply_bounds("crw_map", &json!({ "limit": 50 }), value);
        assert_eq!(out["links"].as_array().unwrap().len(), 50);
        assert_eq!(out["sitemaps"].as_array().unwrap().len(), 50);
        assert_eq!(out["totalDiscovered"], json!(150));
        assert_eq!(out["totalSitemaps"], json!(150));
    }

    // --- strip_credit_fields: additional placements ---

    #[test]
    fn strip_credit_fields_results_item_without_a_data_field_still_strips_its_own_credits() {
        let mut v = json!({ "results": [{ "url": "a", "status": "completed", "creditsUsed": 1 }] });
        strip_credit_fields(&mut v);
        assert!(v["results"][0].get("creditsUsed").is_none());
        assert_eq!(v["results"][0]["url"], json!("a"));
    }

    #[test]
    fn strip_credit_fields_metadata_block_at_top_level_is_stripped() {
        let mut v = json!({ "metadata": { "creditCost": 1, "title": "t" } });
        strip_credit_fields(&mut v);
        assert!(v["metadata"].get("creditCost").is_none());
        assert_eq!(v["metadata"]["title"], json!("t"));
    }

    #[test]
    fn strip_credit_fields_five_levels_deep() {
        let mut v =
            json!({ "a": { "b": { "c": { "d": { "e": { "creditCost": 9, "keep": 1 } } } } } });
        strip_credit_fields(&mut v);
        assert!(v["a"]["b"]["c"]["d"]["e"].get("creditCost").is_none());
        assert_eq!(v["a"]["b"]["c"]["d"]["e"]["keep"], json!(1));
    }

    // --- JsonRpcError / JsonRpcResponse: direct struct-field checks ---

    #[test]
    fn jsonrpc_error_struct_carries_code_and_message_verbatim() {
        let err = JsonRpcError {
            code: -32602,
            message: "invalid params".into(),
        };
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "invalid params");
    }

    #[test]
    fn jsonrpc_response_success_variant_has_none_error_field() {
        let resp = JsonRpcResponse::success(json!(1), json!({}));
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn jsonrpc_response_error_variant_has_none_result_field() {
        let resp = JsonRpcResponse::error(json!(1), -1, "x".into());
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
    }

    // --- JsonRpcRequest: id accepts any JSON type structurally ---

    #[test]
    fn request_id_can_be_a_boolean_value() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#).unwrap();
        assert_eq!(req.id, Some(json!(true)));
    }

    #[test]
    fn request_id_can_be_a_floating_point_number() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).unwrap();
        assert_eq!(req.id, Some(json!(1.5)));
    }

    #[test]
    fn request_id_can_be_an_array_or_object_structurally() {
        // JSON-RPC forbids non-scalar ids, but `Option<Value>` places no such
        // constraint at the deserialization layer; that validation, if any,
        // belongs to a higher layer than this crate's framing types.
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":[1,2],"method":"ping"}"#).unwrap();
        assert_eq!(req.id, Some(json!([1, 2])));
    }

    #[test]
    fn request_jsonrpc_field_wrong_type_errors() {
        let result: Result<JsonRpcRequest, _> =
            serde_json::from_str(r#"{"jsonrpc":2.0,"id":1,"method":"ping"}"#);
        assert!(result.is_err(), "jsonrpc must be a string, not a number");
    }

    #[test]
    fn request_method_field_wrong_type_errors() {
        let result: Result<JsonRpcRequest, _> =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":123}"#);
        assert!(result.is_err(), "method must be a string, not a number");
    }

    #[test]
    fn extract_status_schema_result_item_optional_field_types() {
        let schema = extract_status_output_schema();
        let item = &schema["properties"]["results"]["items"]["properties"];
        assert_eq!(item["llmUsage"]["type"], "object");
        assert_eq!(item["basis"]["type"], "array");
        assert_eq!(item["basisWarnings"]["type"], "array");
        assert_eq!(item["llmInputHash"]["type"], "string");
        assert_eq!(item["error"]["type"], "string");
    }

    #[test]
    fn extract_status_schema_result_item_status_enum_has_four_values() {
        let schema = extract_status_output_schema();
        let enum_vals = schema["properties"]["results"]["items"]["properties"]["status"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(
            enum_vals,
            &vec![
                json!("processing"),
                json!("completed"),
                json!("failed"),
                json!("cancelled")
            ]
        );
    }

    #[test]
    fn extract_status_schema_top_level_expires_at_is_date_time_format() {
        let schema = extract_status_output_schema();
        assert_eq!(schema["properties"]["expiresAt"]["format"], "date-time");
    }

    #[test]
    fn tool_definitions_proxy_and_embedded_modes_have_the_same_tool_names_in_the_same_order() {
        let names = |defs: &Value| -> Vec<String> {
            defs["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(&tool_definitions(false)),
            names(&tool_definitions(true))
        );
    }

    #[test]
    fn crw_search_limit_property_is_an_integer() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        assert_eq!(
            search["inputSchema"]["properties"]["limit"]["type"],
            "integer"
        );
    }

    #[test]
    fn crw_search_lang_and_query_are_strings() {
        let defs = tool_definitions(false);
        let search = tool_by_name(&defs, "crw_search");
        let props = &search["inputSchema"]["properties"];
        assert_eq!(props["lang"]["type"], "string");
        assert_eq!(props["query"]["type"], "string");
    }

    #[test]
    fn extract_accepted_schema_urls_field_is_a_nonnegative_integer() {
        let schema = extract_accepted_output_schema();
        assert_eq!(schema["properties"]["urls"]["type"], "integer");
        assert_eq!(schema["properties"]["urls"]["minimum"], json!(0));
    }

    #[test]
    fn extract_accepted_schema_status_enum_is_processing_only() {
        let schema = extract_accepted_output_schema();
        let enum_vals = schema["properties"]["status"]["enum"].as_array().unwrap();
        assert_eq!(enum_vals, &vec![json!("processing")]);
    }
}
