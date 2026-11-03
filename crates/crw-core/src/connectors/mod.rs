//! Data sources behind the proxy-mode MCP tools `crw_find_data` and
//! `crw_get_data`.
//!
//! Most sources run on the fastCRW API. Some run here instead, on the user's
//! machine and with the user's own key. The API publishes those as a
//! manifest; this module runs them and forwards everything else. The agent
//! sees one tool and one result shape either way.
//!
//! `crw_get_data` routing:
//! - provider in the (unexpired) manifest: run here;
//! - not in it: forward to `GET {api}/v1/data/<p>/<c>`;
//! - the API answers 409 `client_execution_required` (the provider moved since
//!   our fetch): refetch the manifest (unless the copy is under a minute old)
//!   and run here if it is there now; if it is still missing, this crw
//!   rejected it, so the answer is `[connector_misconfigured]` with an upgrade
//!   hint;
//! - no usable manifest: forward nothing, answer `[data_sources_unavailable]`.

#[cfg(feature = "cli")]
pub mod cli;
mod client;
mod execute;
pub mod keys;
pub mod manifest;
pub mod url;

use execute::Outcome;
use manifest::{Connector, Manifest, ManifestCache};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// The two tool names this module answers.
pub const DATA_TOOLS: [&str; 2] = ["crw_find_data", "crw_get_data"];

pub fn is_data_tool(name: &str) -> bool {
    DATA_TOOLS.contains(&name)
}

const SERVER_TIMEOUT: Duration = Duration::from_secs(60);
const FIND_TIMEOUT: Duration = Duration::from_secs(30);
const USAGE_TIMEOUT: Duration = Duration::from_secs(5);
const FIND_DEFAULT_LIMIT: u64 = 10;
const FIND_MAX_LIMIT: u64 = 50;
/// Asks the API to refuse a forwarded result over this many bytes of `data`
/// with 413 `result_too_large`, uncharged, instead of billing a result the MCP
/// layer would have to drop. An API that ignores it answers in full.
const MAX_DATA_BYTES_HEADER: &str = "X-Crw-Max-Data-Bytes";
/// How old a cached manifest may be before a 409 refetches it.
const RECHECK_AFTER: Duration = Duration::from_secs(60);

/// Whether this process is the standalone `crw-mcp` binary rather than `crw`,
/// so a remedy names a command the user has.
pub(crate) fn running_as_crw_mcp() -> bool {
    static IS_MCP: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| {
                p.file_stem()
                    .map(|s| s.to_string_lossy().starts_with("crw-mcp"))
            })
            .unwrap_or(false)
    });
    *IS_MCP
}

/// The `connectors` command of the running binary.
pub fn connectors_command() -> &'static str {
    commands_for(running_as_crw_mcp()).0
}

/// How to upgrade the running binary.
pub(crate) fn upgrade_hint() -> &'static str {
    commands_for(running_as_crw_mcp()).1
}

fn commands_for(crw_mcp: bool) -> (&'static str, &'static str) {
    if crw_mcp {
        (
            "npx -y crw-mcp connectors",
            "npx -y crw-mcp@latest, brew upgrade crw-mcp, or cargo install crw-mcp",
        )
    } else {
        (
            "crw connectors",
            "brew upgrade crw, or cargo install crw-cli",
        )
    }
}

type KeySource = Box<dyn Fn(&str) -> Option<keys::ConnectorEntry> + Send + Sync>;

/// Process-wide state for the data tools: one per MCP server.
pub struct DataToolContext {
    api_url: String,
    api_key: Option<String>,
    telemetry: bool,
    server: reqwest::Client,
    http: client::ConnectorHttp,
    manifest: ManifestCache,
    pacer: execute::Pacer,
    keys: KeySource,
}

/// Whether usage pings are allowed: `[client] telemetry` (or
/// `CRW_CLIENT__TELEMETRY`), on unless set to `false`. A config that does not
/// load counts as off, since the user's choice cannot be read.
pub fn telemetry_enabled() -> bool {
    crate::config::AppConfig::load().is_ok_and(|c| c.client.telemetry != Some(false))
}

impl DataToolContext {
    pub fn new(api_url: String, api_key: Option<String>, telemetry: bool) -> Self {
        Self::build(
            api_url,
            api_key,
            telemetry,
            client::ConnectorHttp::new(client::Guard::default()),
            Box::new(keys::load),
        )
    }

    fn build(
        api_url: String,
        api_key: Option<String>,
        telemetry: bool,
        http: client::ConnectorHttp,
        keys: KeySource,
    ) -> Self {
        // Our API only; the connector client sends each source's own UA.
        let server = reqwest::Client::builder()
            .user_agent(concat!("crw/", env!("CARGO_PKG_VERSION")))
            .redirect(crate::url_safety::safe_redirect_policy())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client builds");
        Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key,
            telemetry,
            server,
            http,
            manifest: ManifestCache::default(),
            pacer: execute::Pacer::default(),
            keys,
        }
    }

    /// The current manifest: cached up to an hour, refetched after that, and
    /// never an expired copy.
    pub async fn manifest(&self) -> Result<Arc<Manifest>, String> {
        self.load_manifest(manifest::MAX_AGE).await
    }

    /// The cached manifest if it is younger than `max_age`, else a fresh one.
    async fn load_manifest(&self, max_age: Duration) -> Result<Arc<Manifest>, String> {
        self.manifest
            .get(
                &self.server,
                &self.api_url,
                self.api_key.as_deref(),
                max_age,
                self.http.guard(),
            )
            .await
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.api_key.as_deref() {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    async fn run_local(
        &self,
        connector: &Connector,
        capability: &str,
        options: &Value,
    ) -> Result<Value, String> {
        let entry = (self.keys)(&connector.provider);
        let run = execute::run(
            &self.http,
            &self.pacer,
            connector,
            capability,
            options,
            entry,
        )
        .await;
        // Only free local runs are capped here. A forwarded call is capped by
        // the API, before it charges (see [`MAX_DATA_BYTES_HEADER`]).
        let result = run.result.and_then(crate::mcp::check_data_payload);
        if let Some((outcome, latency)) = run.usage {
            let outcome = match (&result, outcome) {
                (Err(_), Outcome::Success) => Outcome::ResultTooLarge,
                _ => outcome,
            };
            self.report_usage(&connector.provider, capability, outcome, latency);
        }
        result
    }

    /// Fire-and-forget usage count for a call that ran here. Carries only the
    /// provider, capability, outcome and latency; never awaited, errors only
    /// reach a debug log.
    fn report_usage(&self, provider: &str, capability: &str, outcome: Outcome, latency: Duration) {
        if !self.telemetry {
            return;
        }
        let body = json!({
            "provider": provider,
            "capability": capability,
            "outcome": outcome.as_str(),
            "latencyMs": u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
        });
        let req = self
            .authed(self.server.post(format!("{}/v1/data/usage", self.api_url)))
            .timeout(USAGE_TIMEOUT)
            .json(&body);
        tokio::spawn(async move {
            if let Err(e) = req.send().await {
                tracing::debug!("usage ping failed: {}", crate::error::reqwest_message(e));
            }
        });
    }

    async fn forward(&self, provider: &str, capability: &str, options: &Value) -> Forwarded {
        let params = match execute::params_from(options) {
            Ok(p) => p,
            Err(e) => return Forwarded::Done(Err(e)),
        };
        let Ok(mut target) = ::url::Url::parse(&format!("{}/v1/data", self.api_url)) else {
            return Forwarded::Done(Err(manifest::unavailable("the API URL does not parse")));
        };
        if let Ok(mut segments) = target.path_segments_mut() {
            segments.push(provider).push(capability);
        }
        if !params.is_empty() {
            target.query_pairs_mut().extend_pairs(&params);
        }
        let sent = self
            .authed(self.server.get(target))
            .header(
                MAX_DATA_BYTES_HEADER,
                crate::mcp::MAX_DATA_PAYLOAD_BYTES.to_string(),
            )
            .timeout(SERVER_TIMEOUT)
            .send()
            .await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("data call failed: {}", crate::error::reqwest_message(e));
                return Forwarded::Done(Err(
                    "[server_unreachable] The fastCRW API could not be reached. Try again.".into(),
                ));
            }
        };
        let status = resp.status().as_u16();
        let retry_after = manifest::retry_after_secs(resp.headers());
        let body: Value = resp
            .text()
            .await
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        if status == 409
            && body.get("errorCode").and_then(Value::as_str) == Some("client_execution_required")
        {
            return Forwarded::ClientRequired;
        }
        if !(200..300).contains(&status) || body.get("success") != Some(&Value::Bool(true)) {
            return Forwarded::Done(Err(server_error_text(status, &body, retry_after)));
        }
        Forwarded::Done(Ok(server_envelope(&body)))
    }
}

enum Forwarded {
    Done(Result<Value, String>),
    ClientRequired,
}

/// The API's snake_case data envelope as the camelCase MCP result.
fn server_envelope(body: &Value) -> Value {
    let p = &body["provenance"];
    json!({
        "provider": body["provider"],
        "capability": body["capability"],
        "creditsUsed": body["credits_used"],
        "provenance": {
            "source": p["source"],
            "upstreamUrl": p["upstream_url"],
            "licence": p["licence"],
            "licenceUrl": p["licence_url"],
            "attributionText": p["attribution_text"],
            "retrievedAt": p["retrieved_at"],
        },
        "data": body["data"],
    })
}

/// One bracketed line from an API error body. The code is the body's
/// `errorCode` when it has one (an upstream 404 is `upstream_error`, not
/// `unknown_data_source`), else a bracketed code the message already starts
/// with, else one derived from the HTTP status. A 429's `Retry-After` is
/// carried into the text.
fn server_error_text(status: u16, body: &Value, retry_after: Option<u64>) -> String {
    let message = body
        .get("error")
        .and_then(|e| {
            e.as_str()
                .or_else(|| e.get("message").and_then(Value::as_str))
        })
        .map(|m| m.chars().take(500).collect::<String>())
        .unwrap_or_else(|| format!("The fastCRW API answered HTTP {status}."));
    let is_code = |c: &str| {
        !c.is_empty() && c.len() <= 64 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    };
    let (prefix, message) = match message
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .filter(|(code, _)| is_code(code))
    {
        Some((code, rest)) => (Some(code.to_string()), rest.trim_start().to_string()),
        None => (None, message),
    };
    let code = body
        .get("errorCode")
        .and_then(Value::as_str)
        .filter(|c| is_code(c))
        .map(String::from)
        .or(prefix)
        .unwrap_or_else(|| {
            match status {
                400 => "invalid_options",
                401 | 403 => "unauthorized",
                402 => "insufficient_credits",
                404 => "unknown_data_source",
                429 => "rate_limited",
                _ => "server_error",
            }
            .to_string()
        });
    match retry_after {
        Some(secs) if status == 429 => format!("[{code}] {message} Try again in {secs} seconds."),
        _ => format!("[{code}] {message}"),
    }
}

fn slug_arg<'a>(args: &'a Value, field: &str) -> Result<&'a str, String> {
    let value = args.get(field).and_then(Value::as_str).unwrap_or("");
    let valid = !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(value)
    } else {
        Err(format!(
            "[invalid_options] \"{field}\" must be a lowercase id like the ones crw_find_data returns."
        ))
    }
}

/// The single entry point both proxy dispatchers call.
pub async fn call_data_tool(
    name: &str,
    args: Value,
    ctx: &DataToolContext,
) -> Result<Value, String> {
    match name {
        "crw_find_data" => find_data(&args, ctx).await,
        "crw_get_data" => get_data(&args, ctx).await,
        _ => Err(format!("unknown tool: {name}")),
    }
}

async fn get_data(args: &Value, ctx: &DataToolContext) -> Result<Value, String> {
    let provider = slug_arg(args, "provider")?;
    let capability = slug_arg(args, "capability")?;
    let options = args.get("options").cloned().unwrap_or_else(|| json!({}));
    if !options.is_object() {
        return Err("[invalid_options] options must be an object".into());
    }

    let manifest = ctx.manifest().await?;
    if let Some(connector) = manifest.connector(provider) {
        return ctx.run_local(connector, capability, &options).await;
    }
    match ctx.forward(provider, capability, &options).await {
        Forwarded::Done(result) => result,
        Forwarded::ClientRequired => {
            let fresh = ctx.load_manifest(RECHECK_AFTER).await?;
            match fresh.connector(provider) {
                Some(connector) => ctx.run_local(connector, capability, &options).await,
                // The server runs it here, but this crw dropped it from the
                // manifest; the server's install advice would be wrong.
                None => Err(format!(
                    "[connector_misconfigured] {provider} cannot run on this version of crw. Upgrade crw ({}) and try again.",
                    upgrade_hint()
                )),
            }
        }
    }
}

async fn find_data(args: &Value, ctx: &DataToolContext) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(FIND_DEFAULT_LIMIT)
        .clamp(1, FIND_MAX_LIMIT);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    // Same input rules as the hosted `findData` in crw-saas `dispatch.ts`.
    let text = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let Ok(mut target) = ::url::Url::parse(&format!("{}/v1/data/find", ctx.api_url)) else {
        return Err(manifest::unavailable("the API URL does not parse"));
    };
    {
        let mut query = target.query_pairs_mut();
        if let Some(q) = text("query") {
            query.append_pair("query", q);
        }
        if let Some(c) = text("category") {
            query.append_pair("category", c);
        }
        if let Some(p) = text("provider") {
            query.append_pair("provider", &p.to_lowercase());
        }
        query
            .append_pair("limit", &limit.to_string())
            .append_pair("offset", &offset.to_string());
    }

    let sent = ctx
        .authed(ctx.server.get(target))
        .timeout(FIND_TIMEOUT)
        .send()
        .await
        .map_err(|e| {
            tracing::debug!("find call failed: {}", crate::error::reqwest_message(e));
            "[server_unreachable] The fastCRW API could not be reached. Try again.".to_string()
        })?;
    let status = sent.status().as_u16();
    let retry_after = manifest::retry_after_secs(sent.headers());
    let reply: Value = sent
        .text()
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    // A bare 404 is an API with no data sources (a self-hosted server), the
    // same answer crw_get_data gives for it.
    if status == 404 && reply.get("errorCode").is_none() {
        return Err(manifest::unavailable("the fastCRW API answered HTTP 404"));
    }
    if !(200..300).contains(&status) {
        return Err(server_error_text(status, &reply, retry_after));
    }
    let Some(tools) = reply["items"].as_array() else {
        return Err(
            "[server_error] The fastCRW API returned an unexpected catalogue shape.".into(),
        );
    };

    let has_client = tools.iter().any(|t| t["execution"] == "client");
    let manifest = if has_client {
        ctx.manifest().await.ok()
    } else {
        None
    };
    let items: Vec<Value> = tools
        .iter()
        .map(|t| find_item(t, manifest.as_deref(), ctx))
        .collect();

    let mut out = json!({"items": items, "total": reply["total"]});
    if let Some(next) = reply["nextOffset"].as_u64() {
        out["nextOffset"] = json!(next);
    }
    Ok(out)
}

/// One find-tools item in the MCP shape. Where a call runs is not shown; a
/// source that runs here says whether the user has stored its key.
fn find_item(tool: &Value, manifest: Option<&Manifest>, ctx: &DataToolContext) -> Value {
    let provider = tool["provider"].as_str().unwrap_or("");
    let mut item = json!({
        "provider": tool["provider"],
        "capability": tool["capability"],
        "name": tool["name"],
        "description": tool["description"],
        "creditsCost": tool["creditsCost"],
        "requiredOptions": tool["options"]["required"].as_array().cloned().unwrap_or_default(),
        "optionalOptions": tool["options"]["optional"].as_array().cloned().unwrap_or_default(),
        "keyRequired": tool["keyRequired"].as_bool().unwrap_or(false),
        "licence": tool["provenance"]["licence"].as_str().unwrap_or(""),
    });
    if tool["execution"] == "client" {
        let connector = manifest.and_then(|m| m.connector(provider));
        if connector.is_some_and(|c| c.auth.kind != "none") {
            let configured = (ctx.keys)(provider)
                .and_then(|e| e.api_key)
                .is_some_and(|k| !k.trim().is_empty());
            item["keyConfigured"] = json!(configured);
        }
    }
    item
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod command_tests {
    #[test]
    fn connectors_remedies_name_the_running_binary() {
        assert_eq!(super::commands_for(false).0, "crw connectors");
        assert_eq!(super::commands_for(true).0, "npx -y crw-mcp connectors");
        assert!(super::commands_for(true).1.contains("crw-mcp@latest"));
        assert!(!super::commands_for(false).1.contains("crw-mcp"));
        assert_eq!(super::connectors_command(), "crw connectors");
    }
}
