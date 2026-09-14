//! /map URL filter pipeline.
//!
//! Tier A — drop URL outright if any query key matches the action deny-list.
//! Tier B — strip tracking-param keys from the query, keep the URL.
//! Tier C — host-scoped overrides preserve/exempt/inject extras.
//!
//! Entry points:
//! - [`filter_and_normalize_raw`]: sitemap path (no pre-parsed Url available).
//! - [`filter_and_normalize_parsed`]: BFS path (reuses caller's `url::Url`).
//!
//! Both return `None` if Tier A drops the URL.

use crate::url_filter_data::{
    ALWAYS_PRESERVE, DEFAULT_ACTION_PARAMS, DEFAULT_HOST_OVERRIDES, DEFAULT_TRACKING_PARAMS,
    GOV_TLD_SUFFIXES, HostOverrideEntry, HostPat,
};
use crw_core::metrics::metrics;
use std::collections::HashSet;

/// Canonicalize a query-param key for deny-/preserve-list matching: ASCII
/// lowercase, then fold `-` to `_`. WordPress/WooCommerce plugins emit the
/// same action under either spelling (`add-to-wishlist` vs `add_to_wishlist`,
/// `wc-ajax` vs `wc_ajax`), so folding them onto one canonical form removes
/// the per-variant whack-a-mole (issue #128, follow-up to #40). Both the
/// static lists and every runtime set store canonical keys, and the incoming
/// key is canonicalized before lookup. Matching never alters output — surviving
/// URLs keep their original raw `key=value` pair verbatim.
pub(crate) fn canon_key(k: &str) -> String {
    let mut s = k.to_ascii_lowercase();
    if s.as_bytes().contains(&b'-') {
        s = s.replace('-', "_");
    }
    s
}

/// Per-request override delta. `None` fields fall through to whatever the
/// server's default cfg already specifies. Construction lives in the route
/// handler — it owns the precedence resolution (request > TOML > default).
#[derive(Debug, Clone, Default)]
pub struct RequestOverrides {
    pub strip_tracking: Option<bool>,
    pub drop_actions: Option<bool>,
    /// Firecrawl-compatible coarse alias. Outermost gate: `Some(_)` makes
    /// `strip_tracking` / `drop_actions` ignored.
    pub coarse_strip_all: Option<bool>,
    pub extra_tracking: Option<Vec<String>>,
    pub extra_action: Option<Vec<String>>,
    pub preserve: Option<Vec<String>>,
}

/// Runtime host-override entry (owned strings) built once at config-load time.
#[derive(Debug, Clone)]
pub struct HostOverride {
    pub host_pat: HostPat,
    pub when_path_contains: Vec<String>,
    pub preserve_params: HashSet<String>,
    pub exempt_action_params: HashSet<String>,
    pub extra_tracking_params: HashSet<String>,
}

impl HostOverride {
    fn from_static(e: &HostOverrideEntry) -> Self {
        // `when_path_contains` are path substrings, not param keys — left as-is.
        // The three param sets are canonicalized so `-`/`_` variants match.
        Self {
            host_pat: e.host_pat.clone(),
            when_path_contains: e.when_path_contains.iter().map(|s| s.to_string()).collect(),
            preserve_params: e.preserve_params.iter().map(|s| canon_key(s)).collect(),
            exempt_action_params: e
                .exempt_action_params
                .iter()
                .map(|s| canon_key(s))
                .collect(),
            extra_tracking_params: e
                .extra_tracking_params
                .iter()
                .map(|s| canon_key(s))
                .collect(),
        }
    }
}

/// Filter configuration. Built once at server startup, shared via `Arc`.
#[derive(Debug, Clone)]
pub struct UrlFilterCfg {
    pub strip_tracking: bool,
    pub drop_actions: bool,
    /// Firecrawl-compatible coarse mode: strip every non-preserved param.
    pub coarse_strip_all: bool,
    /// When true, `.gov`/`.mil` etc. hosts run Tier A too. Default false:
    /// gov sites preserve action URLs (govspeak forms etc.).
    pub gov_tld_drop_actions: bool,
    /// User-supplied extras, additive on top of `DEFAULT_TRACKING_PARAMS`.
    /// Pre-lowercased at build time.
    pub tracking_params: HashSet<String>,
    pub action_params: HashSet<String>,
    pub preserve_params: HashSet<String>,
    pub host_overrides: Vec<HostOverride>,
}

impl Default for UrlFilterCfg {
    fn default() -> Self {
        Self::defaults_on()
    }
}

impl UrlFilterCfg {
    /// Plan default: Tier A + Tier B both active, gov suppression on,
    /// coarse mode off, compiled-in host overrides loaded.
    pub fn defaults_on() -> Self {
        Self {
            strip_tracking: true,
            drop_actions: true,
            coarse_strip_all: false,
            gov_tld_drop_actions: false,
            tracking_params: HashSet::new(),
            action_params: HashSet::new(),
            preserve_params: HashSet::new(),
            host_overrides: DEFAULT_HOST_OVERRIDES
                .iter()
                .map(HostOverride::from_static)
                .collect(),
        }
    }

    /// Build from the raw TOML `[map.url_filter]` block. Strings are
    /// canonicalized once here (lowercased, `-` folded to `_`) so per-request
    /// lookups stay allocation-free.
    pub fn from_map_config(cfg: &crw_core::config::MapUrlFilterConfig) -> Self {
        let to_lower_set =
            |v: &[String]| -> HashSet<String> { v.iter().map(|s| canon_key(s)).collect() };
        Self {
            strip_tracking: cfg.strip_tracking_params,
            drop_actions: cfg.drop_action_urls,
            coarse_strip_all: false,
            gov_tld_drop_actions: cfg.gov_tld_drop_actions,
            tracking_params: to_lower_set(&cfg.extra_tracking_params),
            action_params: to_lower_set(&cfg.extra_action_params),
            preserve_params: to_lower_set(&cfg.extra_preserve_params),
            host_overrides: DEFAULT_HOST_OVERRIDES
                .iter()
                .map(HostOverride::from_static)
                .collect(),
        }
    }

    /// Apply request-level overrides on top of `self`. Returns a fresh
    /// `UrlFilterCfg`; the input is not mutated so the server's Arc'd
    /// default stays shareable across concurrent requests.
    pub fn with_overrides(&self, ov: RequestOverrides) -> Self {
        let mut out = self.clone();
        if let Some(coarse) = ov.coarse_strip_all {
            out.coarse_strip_all = coarse;
            if !coarse {
                // Coarse `false` is the explicit "give me raw URLs" escape
                // hatch — switch both tiers off.
                out.strip_tracking = false;
                out.drop_actions = false;
                return out;
            }
            // Coarse `true` overrides granular flags (Tier A still runs).
            return out;
        }
        if let Some(v) = ov.strip_tracking {
            out.strip_tracking = v;
        }
        if let Some(v) = ov.drop_actions {
            out.drop_actions = v;
        }
        if let Some(extra) = ov.extra_tracking {
            for k in extra {
                out.tracking_params.insert(canon_key(&k));
            }
        }
        if let Some(extra) = ov.extra_action {
            for k in extra {
                out.action_params.insert(canon_key(&k));
            }
        }
        if let Some(extra) = ov.preserve {
            for k in extra {
                out.preserve_params.insert(canon_key(&k));
            }
        }
        out
    }

    /// Both tiers off — pass-through (other than `normalize_url`).
    pub fn off() -> Self {
        Self {
            strip_tracking: false,
            drop_actions: false,
            coarse_strip_all: false,
            gov_tld_drop_actions: false,
            tracking_params: HashSet::new(),
            action_params: HashSet::new(),
            preserve_params: HashSet::new(),
            host_overrides: Vec::new(),
        }
    }
}

/// Build a URL dedup key without changing case-sensitive components.
///
/// Scheme and host are case-insensitive. Userinfo, port, path and query remain
/// byte-exact because origins may interpret them case-sensitively.
pub(crate) fn normalize_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let trimmed = without_fragment.trim_end_matches('/');
    let Some(sep) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let authority_start = sep + 3;
    let authority_end = trimmed[authority_start..]
        .find(['/', '?'])
        .map_or(trimmed.len(), |i| authority_start + i);
    let authority = &trimmed[authority_start..authority_end];
    let host_start = authority.rfind('@').map_or(0, |i| i + 1);
    let host_and_port = &authority[host_start..];
    let host_len = if host_and_port.starts_with('[') {
        host_and_port
            .find(']')
            .map_or(host_and_port.len(), |i| i + 1)
    } else {
        host_and_port.rfind(':').unwrap_or(host_and_port.len())
    };
    let host_end = authority_start + host_start + host_len;

    let mut key = trimmed[..sep].to_ascii_lowercase();
    key.push_str("://");
    key.push_str(&trimmed[authority_start..authority_start + host_start]);
    key.push_str(&trimmed[authority_start + host_start..host_end].to_lowercase());
    key.push_str(&trimmed[host_end..authority_end]);
    key.push_str(&trimmed[authority_end..]);
    key
}

/// Returns `host` is matched by any compiled `.gov`/`.mil` suffix.
fn is_gov_host(host: &str) -> bool {
    GOV_TLD_SUFFIXES.iter().any(|suf| {
        if let Some(stripped) = suf.strip_prefix('.') {
            host.eq_ignore_ascii_case(stripped)
                || host.len() > suf.len()
                    && host
                        .get(host.len() - suf.len()..)
                        .map(|t| t.eq_ignore_ascii_case(suf))
                        .unwrap_or(false)
        } else {
            host.eq_ignore_ascii_case(suf)
                || host.len() > suf.len() + 1
                    && host
                        .get(host.len() - suf.len() - 1..)
                        .map(|t| t.eq_ignore_ascii_case(&format!(".{}", suf)))
                        .unwrap_or(false)
        }
    })
}

/// Sitemap entry point — no pre-parsed URL. Cheap pre-screen avoids
/// the `Url::parse` cost for the overwhelming majority of internal links
/// that have no `?`.
pub fn filter_and_normalize_raw(url: &str, cfg: &UrlFilterCfg) -> Option<String> {
    if cfg.coarse_strip_all || cfg.strip_tracking || cfg.drop_actions {
        if !url.contains('?') {
            return Some(normalize_url(url));
        }
        let parsed = match url::Url::parse(url) {
            Ok(u) => u,
            Err(_) => {
                metrics()
                    .map_filter_dropped_total
                    .with_label_values(&["parse_error_passthrough"])
                    .inc();
                return Some(normalize_url(url));
            }
        };
        filter_and_normalize_parsed(&parsed, url, cfg)
    } else {
        Some(normalize_url(url))
    }
}

/// BFS entry point — caller already has a parsed `Url`.
pub fn filter_and_normalize_parsed(
    parsed: &url::Url,
    raw: &str,
    cfg: &UrlFilterCfg,
) -> Option<String> {
    // No query — nothing for either tier to do.
    if parsed.query().is_none() {
        return Some(normalize_url(raw));
    }
    // Both tiers off and no coarse — pass through.
    if !cfg.coarse_strip_all && !cfg.strip_tracking && !cfg.drop_actions {
        return Some(normalize_url(raw));
    }

    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    let path = parsed.path();

    // Resolve effective sets from host overrides.
    let mut eff_preserve: HashSet<String> = cfg.preserve_params.clone();
    let mut eff_exempt_action: HashSet<String> = HashSet::new();
    let mut eff_extra_tracking: HashSet<String> = HashSet::new();
    let mut host_override_hit = false;
    for ov in &cfg.host_overrides {
        if !ov.host_pat.matches(&host) {
            continue;
        }
        let path_ok = ov.when_path_contains.is_empty()
            || ov
                .when_path_contains
                .iter()
                .any(|s| path.contains(s.as_str()));
        if !path_ok {
            continue;
        }
        host_override_hit = true;
        eff_preserve.extend(ov.preserve_params.iter().cloned());
        eff_exempt_action.extend(ov.exempt_action_params.iter().cloned());
        eff_extra_tracking.extend(ov.extra_tracking_params.iter().cloned());
    }

    let gov = is_gov_host(&host);

    // Iterate raw query: preserves original percent-encoding and
    // distinguishes `?k` (no `=`) from `?k=`.
    let raw_query = parsed.query().unwrap_or("");
    let pairs: Vec<(String, &str, bool)> = raw_query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|p| match p.find('=') {
            Some(i) => (canon_key(&p[..i]), p, true),
            None => (canon_key(p), p, false),
        })
        .collect();

    // Tier A: action-URL drop.
    let drop_actions_active = cfg.drop_actions && (cfg.gov_tld_drop_actions || !gov);
    if drop_actions_active {
        for (kl, _raw_pair, _) in &pairs {
            if eff_preserve.contains(kl) || ALWAYS_PRESERVE.contains(kl.as_str()) {
                continue;
            }
            if eff_exempt_action.contains(kl) {
                continue;
            }
            let in_action =
                cfg.action_params.contains(kl) || DEFAULT_ACTION_PARAMS.contains(kl.as_str());
            if in_action {
                metrics()
                    .map_filter_dropped_total
                    .with_label_values(&["action_param"])
                    .inc();
                return None;
            }
        }
    } else if gov && cfg.drop_actions {
        // Tier A suppressed by .gov rule — bookkeep only if an action key was
        // actually present; bounded label cardinality.
        let saw_action = pairs.iter().any(|(kl, _, _)| {
            cfg.action_params.contains(kl) || DEFAULT_ACTION_PARAMS.contains(kl.as_str())
        });
        if saw_action {
            metrics()
                .map_filter_preserved_total
                .with_label_values(&["gov_tld"])
                .inc();
        }
    }

    // Coarse mode — strip everything except preserves.
    if cfg.coarse_strip_all {
        let kept: Vec<&str> = pairs
            .iter()
            .filter(|(kl, _, _)| eff_preserve.contains(kl) || ALWAYS_PRESERVE.contains(kl.as_str()))
            .map(|(_, raw, _)| *raw)
            .collect();
        let stripped_any = kept.len() != pairs.len();
        if stripped_any {
            metrics()
                .map_filter_stripped_total
                .with_label_values(&["coarse_ignore"])
                .inc();
        }
        return Some(rebuild(parsed, &kept));
    }

    // Tier B: tracking strip.
    if cfg.strip_tracking {
        let mut kept: Vec<&str> = Vec::with_capacity(pairs.len());
        let mut stripped_any = false;
        for (kl, raw_pair, _) in &pairs {
            let always_pres = ALWAYS_PRESERVE.contains(kl.as_str());
            let host_pres = eff_preserve.contains(kl);
            if always_pres || host_pres {
                if host_pres && host_override_hit {
                    metrics()
                        .map_filter_preserved_total
                        .with_label_values(&["host_override"])
                        .inc();
                } else if always_pres {
                    metrics()
                        .map_filter_preserved_total
                        .with_label_values(&["always_preserve"])
                        .inc();
                }
                kept.push(raw_pair);
                continue;
            }
            let is_tracking = cfg.tracking_params.contains(kl)
                || DEFAULT_TRACKING_PARAMS.contains(kl.as_str())
                || eff_extra_tracking.contains(kl);
            if is_tracking {
                stripped_any = true;
                continue;
            }
            kept.push(raw_pair);
        }
        if stripped_any {
            metrics()
                .map_filter_stripped_total
                .with_label_values(&["tracking_param"])
                .inc();
        }
        return Some(rebuild(parsed, &kept));
    }

    // Tier A only, no strip configured — return URL with original query intact.
    Some(normalize_url(raw))
}

/// Re-emit URL with the surviving params in original order. Preserves raw
/// percent-encoding; drops the `?` entirely when empty.
fn rebuild(parsed: &url::Url, kept: &[&str]) -> String {
    let mut out = parsed.clone();
    if kept.is_empty() {
        out.set_query(None);
    } else {
        out.set_query(Some(&kept.join("&")));
    }
    out.set_fragment(None);
    normalize_url(out.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_on() -> UrlFilterCfg {
        UrlFilterCfg::defaults_on()
    }

    #[test]
    fn no_query_fast_path() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://example.com/foo", &cfg).unwrap();
        assert_eq!(out, "https://example.com/foo");
    }

    #[test]
    fn action_param_drops_url() {
        let cfg = cfg_on();
        assert!(
            filter_and_normalize_raw("https://shop.example.com/?add-to-cart=360", &cfg).is_none()
        );
    }

    #[test]
    fn wpnonce_drops_url() {
        let cfg = cfg_on();
        assert!(
            filter_and_normalize_raw(
                "https://shop.example.com/?add_to_wishlist=6241&_wpnonce=b7643da9b9",
                &cfg
            )
            .is_none()
        );
    }

    #[test]
    fn case_insensitive_action_key() {
        let cfg = cfg_on();
        for u in [
            "https://e.test/?ADD-TO-CART=1",
            "https://e.test/?Add-To-Cart=1",
            "https://e.test/?add-to-cart=1",
        ] {
            assert!(
                filter_and_normalize_raw(u, &cfg).is_none(),
                "expected drop for {u}"
            );
        }
    }

    #[test]
    fn tracking_param_stripped_url_kept() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://example.com/blog?utm_source=fb&fbclid=abc", &cfg)
                .unwrap();
        assert_eq!(out, "https://example.com/blog");
    }

    #[test]
    fn always_preserve_keys_survive() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://wp.example.com/?p=123&utm_source=x", &cfg).unwrap();
        assert!(out.contains("p=123"), "got {out}");
        assert!(!out.contains("utm_source"), "got {out}");
    }

    #[test]
    fn action_wins_when_coexists_with_tracking() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=1&utm_source=x", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn empty_query_after_strip_drops_question_mark() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://example.com/blog?utm_source=fb", &cfg).unwrap();
        assert_eq!(out, "https://example.com/blog");
    }

    #[test]
    fn empty_value_action_key() {
        // ?_wpnonce — no equals sign — should still drop.
        let cfg = cfg_on();
        assert!(filter_and_normalize_raw("https://e.test/?_wpnonce", &cfg).is_none());
    }

    #[test]
    fn repeated_tracking_keys_all_stripped() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://e.test/blog?utm_source=a&utm_source=b&p=1", &cfg)
                .unwrap();
        assert!(out.contains("p=1"));
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn malformed_url_passthrough() {
        let cfg = cfg_on();
        // Has `?` so it triggers the parse path; "not a url" fails parse.
        let out = filter_and_normalize_raw("not-a-url?utm=1", &cfg);
        assert!(out.is_some());
    }

    #[test]
    fn host_override_phpbb_preserves_thread_ids() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://forum.example.com/viewtopic.php?t=123&utm_source=x",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("t=123"), "got {out}");
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn gov_tld_tier_a_suppressed_tier_b_runs() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://senate.gov/?docid=123&utm_source=x&add-to-cart=1",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("docid=123"), "got {out}");
        assert!(out.contains("add-to-cart=1"), "got {out}");
        assert!(!out.contains("utm_source"), "got {out}");
    }

    #[test]
    fn gov_tld_opt_in_runs_tier_a() {
        let mut cfg = cfg_on();
        cfg.gov_tld_drop_actions = true;
        let out = filter_and_normalize_raw("https://senate.gov/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn shopify_host_strips_storefront_keys() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://shop.myshopify.com/products/x?_pos=1&_sid=abc&_v=1.0",
            &cfg,
        )
        .unwrap();
        assert!(!out.contains("_pos"));
        assert!(!out.contains("_sid"));
        assert!(!out.contains("_v="));
    }

    #[test]
    fn shopify_keys_pass_through_on_other_hosts() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://random.com/?_pos=1", &cfg).unwrap();
        assert!(out.contains("_pos=1"));
    }

    #[test]
    fn youtube_watch_preserves_v_list_t() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://www.youtube.com/watch?v=abc123&list=PL1&utm_source=x",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("v=abc123"), "got {out}");
        assert!(
            out.contains("list=pl1") || out.contains("list=PL1"),
            "got {out}"
        );
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn coarse_strip_all_drops_everything_except_preserve() {
        let mut cfg = cfg_on();
        cfg.coarse_strip_all = true;
        let out = filter_and_normalize_raw("https://e.test/blog?p=1&random=foo&utm_source=x", &cfg)
            .unwrap();
        assert!(out.contains("p=1"));
        assert!(!out.contains("random"));
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn coarse_mode_still_runs_tier_a() {
        let mut cfg = cfg_on();
        cfg.coarse_strip_all = true;
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn off_config_returns_raw_normalized() {
        let cfg = UrlFilterCfg::off();
        let out = filter_and_normalize_raw("https://e.test/blog?utm_source=x&add-to-cart=1", &cfg)
            .unwrap();
        assert!(out.contains("utm_source=x"));
        assert!(out.contains("add-to-cart=1"));
    }

    #[test]
    fn extra_action_param_drops_url() {
        let mut cfg = cfg_on();
        cfg.action_params.insert("custom_action".to_string());
        let out = filter_and_normalize_raw("https://e.test/?custom_action=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn extra_preserve_protects_against_default_action() {
        let mut cfg = cfg_on();
        // Runtime sets store canonical keys (`-` folded to `_`); both spellings
        // of the incoming key resolve to this entry.
        cfg.preserve_params.insert("add_to_cart".to_string());
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=1", &cfg).unwrap();
        assert!(out.contains("add-to-cart=1"));
    }

    // ─────────────── issue #128: hyphen/underscore fold ───────────────

    /// Hyphen spelling of a list entry stored in underscore form still drops.
    /// `DEFAULT_ACTION_PARAMS` has `add_to_wishlist`; the live site emitted
    /// `?add-to-wishlist=` and it leaked through before the fold.
    #[test]
    fn wishlist_hyphen_variant_drops() {
        let cfg = cfg_on();
        assert!(
            filter_and_normalize_raw("https://www.urbanboxco.com/?add-to-wishlist=1405", &cfg)
                .is_none()
        );
    }

    /// Compare-plugin action param (`?add_to_compare=`) drops, in both spellings.
    #[test]
    fn compare_action_param_drops() {
        let cfg = cfg_on();
        for u in [
            "https://www.urbanboxco.com/?add_to_compare=1405",
            "https://www.urbanboxco.com/?add-to-compare=1405",
            "https://www.urbanboxco.com/blog/?add_to_compare=2599",
        ] {
            assert!(
                filter_and_normalize_raw(u, &cfg).is_none(),
                "expected drop for {u}"
            );
        }
    }

    /// WPC Smart Compare remove action drops on its own, without a co-occurring
    /// `_wpnonce` to carry it (the live issue-128 site always paired them).
    #[test]
    fn remove_compare_item_drops_without_nonce() {
        let cfg = cfg_on();
        assert!(
            filter_and_normalize_raw(
                "https://www.urbanboxco.com/?remove_compare_item=abc123",
                &cfg
            )
            .is_none()
        );
    }

    /// Mixed-separator spelling of `wc-ajax`/`wc_ajax` both drop.
    #[test]
    fn wc_ajax_both_spellings_drop() {
        let cfg = cfg_on();
        assert!(
            filter_and_normalize_raw("https://e.test/?wc-ajax=get_refreshed_fragments", &cfg)
                .is_none()
        );
        assert!(
            filter_and_normalize_raw("https://e.test/?wc_ajax=get_refreshed_fragments", &cfg)
                .is_none()
        );
    }

    /// Tracking key with a hyphen in its canonical entry (`utm_social_type`)
    /// strips regardless of the incoming separator.
    #[test]
    fn tracking_hyphen_variant_stripped() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://e.test/blog?utm_social-type=x&p=1", &cfg).unwrap();
        assert!(!out.contains("utm_social"), "got {out}");
        assert!(out.contains("p=1"), "got {out}");
    }

    #[test]
    fn fragment_stripped() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://e.test/blog?utm_source=x#section", &cfg).unwrap();
        assert!(!out.contains('#'));
    }

    // ─────────────────────────── property tests ──────────────────────────
    use proptest::prelude::*;

    /// Generate a query-string-safe ASCII key.
    fn arb_key() -> impl Strategy<Value = String> {
        "[a-zA-Z_][a-zA-Z0-9_-]{0,15}".prop_map(String::from)
    }

    fn arb_pair() -> impl Strategy<Value = (String, String)> {
        (arb_key(), "[a-zA-Z0-9._~-]{0,12}".prop_map(String::from))
    }

    proptest! {
        /// Output is either `None` (dropped) or a parseable URL.
        #[test]
        fn output_always_valid(pairs in prop::collection::vec(arb_pair(), 0..6)) {
            let cfg = cfg_on();
            let q: String = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            let url = if q.is_empty() {
                "https://example.com/path".to_string()
            } else {
                format!("https://example.com/path?{q}")
            };
            if let Some(out) = filter_and_normalize_raw(&url, &cfg) {
                prop_assert!(url::Url::parse(&out).is_ok(), "output not a valid URL: {out}");
            }
        }

        /// Idempotency: filter ∘ filter == filter.
        #[test]
        fn filter_idempotent(pairs in prop::collection::vec(arb_pair(), 0..6)) {
            let cfg = cfg_on();
            let q: String = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            let url = if q.is_empty() {
                "https://example.com/path".to_string()
            } else {
                format!("https://example.com/path?{q}")
            };
            let once = filter_and_normalize_raw(&url, &cfg);
            if let Some(o) = &once {
                let twice = filter_and_normalize_raw(o, &cfg);
                prop_assert_eq!(twice.as_ref(), Some(o));
            }
        }

        /// Length non-increasing: surviving query ≤ input query length.
        #[test]
        fn length_non_increasing(pairs in prop::collection::vec(arb_pair(), 0..6)) {
            let cfg = cfg_on();
            let q: String = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            if q.is_empty() {
                return Ok(());
            }
            let url = format!("https://example.com/path?{q}");
            if let Some(out) = filter_and_normalize_raw(&url, &cfg) {
                let out_q_len = url::Url::parse(&out)
                    .ok()
                    .and_then(|u| u.query().map(|s| s.len()))
                    .unwrap_or(0);
                prop_assert!(out_q_len <= q.len(), "{out_q_len} > {len}", len = q.len());
            }
        }
    }

    #[test]
    fn session_keys_strip_not_drop() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://legacy.example.com/page?jsessionid=ABC&p=1", &cfg)
                .unwrap();
        assert!(!out.contains("jsessionid"));
        assert!(out.contains("p=1"));
    }

    // ─────────────────────────── canon_key ───────────────────────────

    #[test]
    fn canon_key_lowercases() {
        assert_eq!(canon_key("UTM_SOURCE"), "utm_source");
    }

    #[test]
    fn canon_key_folds_single_hyphen() {
        assert_eq!(canon_key("add-to-cart"), "add_to_cart");
    }

    #[test]
    fn canon_key_folds_multiple_hyphens() {
        assert_eq!(canon_key("a-b-c-d"), "a_b_c_d");
    }

    #[test]
    fn canon_key_underscore_only_unchanged() {
        assert_eq!(canon_key("already_snake"), "already_snake");
    }

    #[test]
    fn canon_key_mixed_case_and_hyphen() {
        assert_eq!(canon_key("Add-To-Wishlist"), "add_to_wishlist");
    }

    #[test]
    fn canon_key_empty_string() {
        assert_eq!(canon_key(""), "");
    }

    #[test]
    fn canon_key_noop_when_already_canonical() {
        assert_eq!(canon_key("p"), "p");
    }

    #[test]
    fn canon_key_numeric_unchanged() {
        assert_eq!(canon_key("utm_id_123"), "utm_id_123");
    }

    #[test]
    fn canon_key_is_idempotent() {
        let once = canon_key("Add-To-Cart");
        let twice = canon_key(&once);
        assert_eq!(once, twice);
    }

    // ─────────────────────────── is_gov_host ───────────────────────────

    #[test]
    fn is_gov_host_exact_gov() {
        assert!(is_gov_host("senate.gov"));
    }

    #[test]
    fn is_gov_host_subdomain_gov() {
        assert!(is_gov_host("www.senate.gov"));
    }

    #[test]
    fn is_gov_host_rejects_non_gov() {
        assert!(!is_gov_host("example.com"));
    }

    #[test]
    fn is_gov_host_gov_uk_suffix() {
        assert!(is_gov_host("parliament.gov.uk"));
    }

    #[test]
    fn is_gov_host_mil_suffix() {
        assert!(is_gov_host("army.mil"));
    }

    #[test]
    fn is_gov_host_europa_eu_exact() {
        assert!(is_gov_host("europa.eu"));
    }

    #[test]
    fn is_gov_host_europa_eu_subdomain() {
        assert!(is_gov_host("ec.europa.eu"));
    }

    #[test]
    fn is_gov_host_rejects_suffix_without_dot_boundary() {
        // "badeuropa.eu" ends in the same letters as "europa.eu" but with no
        // '.' boundary before it, so it must not match.
        assert!(!is_gov_host("badeuropa.eu"));
    }

    #[test]
    fn is_gov_host_case_insensitive() {
        assert!(is_gov_host("SENATE.GOV"));
    }

    #[test]
    fn is_gov_host_rejects_lookalike_domain() {
        assert!(!is_gov_host("govtjobs.com"));
    }

    // ─────────────── UrlFilterCfg::defaults_on / off / from_map_config ───────────────

    #[test]
    fn defaults_on_field_values() {
        let cfg = UrlFilterCfg::defaults_on();
        assert!(cfg.strip_tracking);
        assert!(cfg.drop_actions);
        assert!(!cfg.coarse_strip_all);
        assert!(!cfg.gov_tld_drop_actions);
        assert!(cfg.tracking_params.is_empty());
        assert!(cfg.action_params.is_empty());
        assert!(cfg.preserve_params.is_empty());
    }

    #[test]
    fn defaults_on_loads_all_compiled_host_overrides() {
        let cfg = UrlFilterCfg::defaults_on();
        assert_eq!(cfg.host_overrides.len(), DEFAULT_HOST_OVERRIDES.len());
    }

    #[test]
    fn off_disables_both_tiers() {
        let cfg = UrlFilterCfg::off();
        assert!(!cfg.strip_tracking);
        assert!(!cfg.drop_actions);
        assert!(!cfg.coarse_strip_all);
        assert!(!cfg.gov_tld_drop_actions);
    }

    #[test]
    fn off_has_no_host_overrides() {
        let cfg = UrlFilterCfg::off();
        assert!(cfg.host_overrides.is_empty());
        assert!(cfg.tracking_params.is_empty());
        assert!(cfg.action_params.is_empty());
        assert!(cfg.preserve_params.is_empty());
    }

    #[test]
    fn default_trait_matches_defaults_on() {
        let via_default = UrlFilterCfg::default();
        let via_ctor = UrlFilterCfg::defaults_on();
        assert_eq!(via_default.strip_tracking, via_ctor.strip_tracking);
        assert_eq!(via_default.drop_actions, via_ctor.drop_actions);
        assert_eq!(
            via_default.host_overrides.len(),
            via_ctor.host_overrides.len()
        );
    }

    #[test]
    fn from_map_config_maps_bools_through() {
        let raw = crw_core::config::MapUrlFilterConfig {
            strip_tracking_params: false,
            drop_action_urls: true,
            gov_tld_drop_actions: true,
            extra_tracking_params: vec![],
            extra_action_params: vec![],
            extra_preserve_params: vec![],
        };
        let cfg = UrlFilterCfg::from_map_config(&raw);
        assert!(!cfg.strip_tracking);
        assert!(cfg.drop_actions);
        assert!(cfg.gov_tld_drop_actions);
        assert!(!cfg.coarse_strip_all);
    }

    #[test]
    fn from_map_config_canonicalizes_extra_params() {
        let raw = crw_core::config::MapUrlFilterConfig {
            strip_tracking_params: true,
            drop_action_urls: true,
            gov_tld_drop_actions: false,
            extra_tracking_params: vec!["My-Tracker".to_string()],
            extra_action_params: vec!["Custom-Action".to_string()],
            extra_preserve_params: vec!["Keep-Me".to_string()],
        };
        let cfg = UrlFilterCfg::from_map_config(&raw);
        assert!(cfg.tracking_params.contains("my_tracker"));
        assert!(cfg.action_params.contains("custom_action"));
        assert!(cfg.preserve_params.contains("keep_me"));
    }

    // ─────────────────────────── with_overrides ───────────────────────────

    #[test]
    fn with_overrides_no_change_when_all_none() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides::default());
        assert_eq!(out.strip_tracking, base.strip_tracking);
        assert_eq!(out.drop_actions, base.drop_actions);
        assert_eq!(out.coarse_strip_all, base.coarse_strip_all);
    }

    #[test]
    fn with_overrides_coarse_true_sets_flag() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            coarse_strip_all: Some(true),
            ..Default::default()
        });
        assert!(out.coarse_strip_all);
    }

    #[test]
    fn with_overrides_coarse_true_short_circuits_granular_flags_in_same_call() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            coarse_strip_all: Some(true),
            strip_tracking: Some(false),
            drop_actions: Some(false),
            ..Default::default()
        });
        // Coarse=true returns early; strip_tracking/drop_actions from the
        // same call are never applied (they still hold the base's values).
        assert_eq!(out.strip_tracking, base.strip_tracking);
        assert_eq!(out.drop_actions, base.drop_actions);
    }

    #[test]
    fn with_overrides_coarse_false_forces_both_tiers_off() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            coarse_strip_all: Some(false),
            ..Default::default()
        });
        assert!(!out.strip_tracking);
        assert!(!out.drop_actions);
        assert!(!out.coarse_strip_all);
    }

    #[test]
    fn with_overrides_coarse_false_ignores_granular_overrides_in_same_call() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            coarse_strip_all: Some(false),
            strip_tracking: Some(true),
            drop_actions: Some(true),
            ..Default::default()
        });
        // coarse=false is the "give me raw URLs" escape hatch and wins over
        // any granular flags supplied in the same request.
        assert!(!out.strip_tracking);
        assert!(!out.drop_actions);
    }

    #[test]
    fn with_overrides_strip_tracking_alone() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            strip_tracking: Some(false),
            ..Default::default()
        });
        assert!(!out.strip_tracking);
        assert!(out.drop_actions); // untouched
    }

    #[test]
    fn with_overrides_drop_actions_alone() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            drop_actions: Some(false),
            ..Default::default()
        });
        assert!(!out.drop_actions);
        assert!(out.strip_tracking); // untouched
    }

    #[test]
    fn with_overrides_extra_tracking_canonicalized() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["My-Tracker".to_string()]),
            ..Default::default()
        });
        assert!(out.tracking_params.contains("my_tracker"));
    }

    #[test]
    fn with_overrides_extra_action_canonicalized() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            extra_action: Some(vec!["Custom-Action".to_string()]),
            ..Default::default()
        });
        assert!(out.action_params.contains("custom_action"));
    }

    #[test]
    fn with_overrides_preserve_canonicalized() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            preserve: Some(vec!["Keep-Me".to_string()]),
            ..Default::default()
        });
        assert!(out.preserve_params.contains("keep_me"));
    }

    #[test]
    fn with_overrides_combines_multiple_extras_in_one_call() {
        let base = cfg_on();
        let out = base.with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["t1".to_string()]),
            extra_action: Some(vec!["a1".to_string()]),
            preserve: Some(vec!["p1".to_string()]),
            ..Default::default()
        });
        assert!(out.tracking_params.contains("t1"));
        assert!(out.action_params.contains("a1"));
        assert!(out.preserve_params.contains("p1"));
    }

    #[test]
    fn with_overrides_does_not_mutate_original() {
        let base = cfg_on();
        let base_tracking_len = base.tracking_params.len();
        let _ = base.with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["new_key".to_string()]),
            ..Default::default()
        });
        assert_eq!(base.tracking_params.len(), base_tracking_len);
        assert!(!base.tracking_params.contains("new_key"));
    }

    #[test]
    fn with_overrides_extras_are_additive_not_replacing() {
        let base = cfg_on().with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["first".to_string()]),
            ..Default::default()
        });
        let out = base.with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["second".to_string()]),
            ..Default::default()
        });
        assert!(out.tracking_params.contains("first"));
        assert!(out.tracking_params.contains("second"));
    }

    #[test]
    fn with_overrides_functional_coarse_false_end_to_end() {
        let cfg = cfg_on().with_overrides(RequestOverrides {
            coarse_strip_all: Some(false),
            ..Default::default()
        });
        // Neither Tier A nor Tier B run: an action param survives raw.
        let out =
            filter_and_normalize_raw("https://e.test/?add-to-cart=1&utm_source=x", &cfg).unwrap();
        assert!(out.contains("add-to-cart=1"));
        assert!(out.contains("utm_source=x"));
    }

    #[test]
    fn with_overrides_functional_extra_action_end_to_end() {
        let cfg = cfg_on().with_overrides(RequestOverrides {
            extra_action: Some(vec!["mycustomaction".to_string()]),
            ..Default::default()
        });
        let out = filter_and_normalize_raw("https://e.test/?mycustomaction=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn with_overrides_functional_preserve_rescues_default_tracking_param() {
        let cfg = cfg_on().with_overrides(RequestOverrides {
            preserve: Some(vec!["utm_source".to_string()]),
            ..Default::default()
        });
        let out = filter_and_normalize_raw("https://e.test/blog?utm_source=x&p=1", &cfg).unwrap();
        assert!(out.contains("utm_source=x"), "got {out}");
    }

    // ─────────────── custom HostOverride: branches the default list never exercises ───────────────

    #[test]
    fn custom_exempt_action_param_survives_tier_a_on_matching_host() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("special.example.com"),
            when_path_contains: vec![],
            preserve_params: HashSet::new(),
            exempt_action_params: HashSet::from(["add_to_cart".to_string()]),
            extra_tracking_params: HashSet::new(),
        });
        let out = filter_and_normalize_raw("https://special.example.com/?add-to-cart=1", &cfg);
        assert!(out.is_some(), "exempted action param must not drop the URL");
        assert!(out.unwrap().contains("add-to-cart=1"));
    }

    #[test]
    fn custom_exempt_action_param_only_applies_on_matching_host() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("special.example.com"),
            when_path_contains: vec![],
            preserve_params: HashSet::new(),
            exempt_action_params: HashSet::from(["add_to_cart".to_string()]),
            extra_tracking_params: HashSet::new(),
        });
        // A different host with the same param still drops.
        let out = filter_and_normalize_raw("https://other.example.com/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn custom_preserve_rescues_tracking_param_on_matching_host() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("rescue.example.com"),
            when_path_contains: vec![],
            preserve_params: HashSet::from(["utm_source".to_string()]),
            exempt_action_params: HashSet::new(),
            extra_tracking_params: HashSet::new(),
        });
        let out =
            filter_and_normalize_raw("https://rescue.example.com/?utm_source=x&p=1", &cfg).unwrap();
        assert!(out.contains("utm_source=x"), "got {out}");
    }

    #[test]
    fn custom_host_override_gated_off_when_path_does_not_match() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("gated.example.com"),
            when_path_contains: vec!["/special/".to_string()],
            preserve_params: HashSet::from(["utm_source".to_string()]),
            exempt_action_params: HashSet::new(),
            extra_tracking_params: HashSet::new(),
        });
        // Path does not contain "/special/" — override never activates,
        // so utm_source is stripped like anywhere else.
        let out =
            filter_and_normalize_raw("https://gated.example.com/other?utm_source=x&p=1", &cfg)
                .unwrap();
        assert!(!out.contains("utm_source"), "got {out}");
    }

    #[test]
    fn custom_host_override_activates_when_path_matches() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("gated.example.com"),
            when_path_contains: vec!["/special/".to_string()],
            preserve_params: HashSet::from(["utm_source".to_string()]),
            exempt_action_params: HashSet::new(),
            extra_tracking_params: HashSet::new(),
        });
        let out = filter_and_normalize_raw(
            "https://gated.example.com/special/page?utm_source=x&p=1",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("utm_source=x"), "got {out}");
    }

    #[test]
    fn custom_extra_tracking_strips_only_on_matching_host() {
        let mut cfg = UrlFilterCfg::defaults_on();
        cfg.host_overrides.push(HostOverride {
            host_pat: HostPat::Exact("tracked.example.com"),
            when_path_contains: vec![],
            preserve_params: HashSet::new(),
            exempt_action_params: HashSet::new(),
            extra_tracking_params: HashSet::from(["myspecialtrack".to_string()]),
        });
        let stripped =
            filter_and_normalize_raw("https://tracked.example.com/?myspecialtrack=1&p=1", &cfg)
                .unwrap();
        assert!(!stripped.contains("myspecialtrack"), "got {stripped}");
        assert!(stripped.contains("p=1"));

        let kept =
            filter_and_normalize_raw("https://other.example.com/?myspecialtrack=1&p=1", &cfg)
                .unwrap();
        assert!(kept.contains("myspecialtrack"), "got {kept}");
    }

    #[test]
    fn default_config_unions_two_simultaneously_matching_host_overrides() {
        // "/wiki/viewtopic.php" matches both the phpBB entry
        // (when_path_contains "viewtopic.php") and the MediaWiki entry
        // (when_path_contains "/wiki/"); both are HostPat::Any so both fire
        // on the same host, and their preserve sets should union.
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://example.com/wiki/viewtopic.php?t=5&title=X&utm_source=y",
            &cfg,
        )
        .unwrap();
        // `normalize_url()` folds only scheme and host, so the preserved query
        // value keeps the case the caller wrote.
        assert!(out.contains("t=5"), "phpBB preserve missing: {out}");
        assert!(out.contains("title=X"), "wiki preserve missing: {out}");
        assert!(!out.contains("utm_source"), "got {out}");
    }

    // ─────────────── default host-override entries: remaining path substrings ───────────────

    #[test]
    fn phpbb_override_matches_showthread_php() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://forum.example.com/showthread.php?t=99&utm_source=x",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("t=99"), "got {out}");
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn phpbb_override_matches_showtopic_substring() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://forum.example.com/index.php?showtopic=42&utm_source=x",
            &cfg,
        )
        .unwrap();
        assert!(!out.contains("utm_source"), "got {out}");
    }

    #[test]
    fn normalize_folds_only_scheme_and_host() {
        assert_eq!(
            normalize_url("HTTPS://User:PaSS@EXAMPLE.COM:8443/Docs/Guide?Id=AbC"),
            "https://User:PaSS@example.com:8443/Docs/Guide?Id=AbC"
        );
        assert_eq!(
            normalize_url("HTTPS://User:PaSS@[ABCD::1]:8443/Path"),
            "https://User:PaSS@[abcd::1]:8443/Path"
        );
        assert_eq!(
            normalize_url("HTTPS://BÜCHER.example/Path"),
            "https://bücher.example/Path"
        );
    }

    #[test]
    fn normalize_trailing_slash_before_a_query_is_untouched() {
        assert_eq!(
            normalize_url("https://example.com/a/?q=1"),
            "https://example.com/a/?q=1"
        );
    }

    #[test]
    fn filter_preserves_case_sensitive_userinfo() {
        let output = filter_and_normalize_raw(
            "HTTPS://User:PaSS@EXAMPLE.COM:8443/Path?Token=AbC",
            &UrlFilterCfg::off(),
        )
        .unwrap();
        assert_eq!(output, "https://User:PaSS@example.com:8443/Path?Token=AbC");
    }

    #[test]
    fn wiki_override_matches_w_index_php_substring() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://wiki.example.com/w/index.php?title=Main&utm_source=x",
            &cfg,
        )
        .unwrap();
        // `normalize_url()` folds only scheme and host. MediaWiki titles are
        // case-sensitive, so preserving the
        // case here is what makes the returned URL resolve.
        assert!(out.contains("title=Main"), "got {out}");
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn youtube_bare_host_exact_match() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://youtube.com/watch?v=abc&utm_source=x", &cfg).unwrap();
        assert!(out.contains("v=abc"), "got {out}");
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn reddit_override_preserves_context_sort_depth() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://old.reddit.com/r/rust/comments/abc/title/?context=3&sort=top&depth=1&utm_source=x",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("context=3"), "got {out}");
        assert!(out.contains("sort=top"), "got {out}");
        assert!(out.contains("depth=1"), "got {out}");
        assert!(!out.contains("utm_source"));
    }

    // ─────────────────────────── gov TLD combinations ───────────────────────────

    #[test]
    fn gov_host_plain_param_unaffected() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://senate.gov/?p=1", &cfg).unwrap();
        assert_eq!(out, "https://senate.gov/?p=1");
    }

    #[test]
    fn gov_tld_opt_in_runs_tier_a_on_gov_uk() {
        let mut cfg = cfg_on();
        cfg.gov_tld_drop_actions = true;
        let out = filter_and_normalize_raw("https://parliament.gov.uk/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn gov_tld_opt_in_runs_tier_a_on_mil() {
        let mut cfg = cfg_on();
        cfg.gov_tld_drop_actions = true;
        let out = filter_and_normalize_raw("https://army.mil/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    #[test]
    fn gov_tld_flag_does_not_affect_non_gov_hosts() {
        let mut cfg = cfg_on();
        cfg.gov_tld_drop_actions = true;
        // Non-gov host: flag is irrelevant, action param still drops (Tier A
        // always runs off-gov).
        let out = filter_and_normalize_raw("https://shop.example.com/?add-to-cart=1", &cfg);
        assert!(out.is_none());
    }

    // ─────────────────────────── rebuild / direct filter_and_normalize_parsed ───────────────────────────

    #[test]
    fn rebuild_preserves_surviving_param_order() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://e.test/blog?z=1&utm_source=x&a=2", &cfg).unwrap();
        let z_pos = out.find("z=1").unwrap();
        let a_pos = out.find("a=2").unwrap();
        assert!(z_pos < a_pos, "order not preserved: {out}");
    }

    #[test]
    fn rebuild_preserves_percent_encoding_in_surviving_value() {
        let cfg = cfg_on();
        let out =
            filter_and_normalize_raw("https://e.test/search?q=hello%20world&utm_source=x", &cfg)
                .unwrap();
        assert!(out.contains("q=hello%20world"), "got {out}");
    }

    #[test]
    fn filter_and_normalize_parsed_no_query_branch() {
        let cfg = cfg_on();
        let parsed = url::Url::parse("https://example.com/plain").unwrap();
        let out = filter_and_normalize_parsed(&parsed, "https://example.com/plain", &cfg);
        assert_eq!(out.unwrap(), "https://example.com/plain");
    }

    #[test]
    fn filter_and_normalize_parsed_both_tiers_off_passthrough() {
        let cfg = UrlFilterCfg::off();
        let raw = "https://example.com/?utm_source=x&add-to-cart=1";
        let parsed = url::Url::parse(raw).unwrap();
        let out = filter_and_normalize_parsed(&parsed, raw, &cfg).unwrap();
        assert!(out.contains("utm_source=x"));
        assert!(out.contains("add-to-cart=1"));
    }

    // ─────────────────────────── malformed / unusual query input ───────────────────────────

    #[test]
    fn trailing_question_mark_no_params() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://example.com/page?", &cfg).unwrap();
        assert_eq!(out, "https://example.com/page");
    }

    #[test]
    fn nested_url_in_query_value_not_confused_with_extra_params() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw(
            "https://e.test/go?redirect=https://other.com?x=1&utm_source=y",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("redirect="), "got {out}");
        assert!(!out.contains("utm_source"), "got {out}");
    }

    #[test]
    fn many_params_no_panic() {
        let cfg = cfg_on();
        let q: Vec<String> = (0..200).map(|i| format!("k{i}=v{i}")).collect();
        let url = format!("https://e.test/big?{}", q.join("&"));
        let out = filter_and_normalize_raw(&url, &cfg);
        assert!(out.is_some());
    }

    #[test]
    fn unicode_key_and_value_no_panic() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?名前=太郎&p=1", &cfg);
        assert!(out.is_some());
    }

    #[test]
    fn empty_pair_between_ampersands_ignored() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?&p=1", &cfg).unwrap();
        assert!(out.contains("p=1"));
    }

    #[test]
    fn semicolon_is_not_a_pair_separator() {
        // Modern URL parsing treats ';' as part of the value, not a separator.
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?p=1;q=2", &cfg).unwrap();
        assert!(out.contains("p=1;q=2"), "got {out}");
    }

    #[test]
    fn plus_sign_in_query_value_not_decoded() {
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?p=a+b", &cfg).unwrap();
        assert!(out.contains("p=a+b"), "got {out}");
    }

    #[test]
    fn percent_encoded_action_key_bypasses_tier_a() {
        // BUG: canon_key() folds '-' to '_' on the *raw* (still percent-encoded)
        // key text — it never percent-decodes first. A hyphen written as
        // `%2D` therefore never becomes `_`, so this action param does not
        // match `add_to_cart` in DEFAULT_ACTION_PARAMS and slips through
        // Tier A entirely. Documenting current (bypassing) behavior; not
        // fixed here per the test-expansion rules (production code untouched).
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?add%2Dto%2Dcart=1", &cfg);
        assert!(
            out.is_some(),
            "percent-encoded hyphen currently bypasses the action-param filter"
        );
    }

    #[test]
    fn action_param_with_url_encoded_value_still_drops() {
        // Sanity check contrasting the key-encoding gap above: an ordinary
        // (unencoded) action key still drops even with an encoded value.
        let cfg = cfg_on();
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=hello%20world", &cfg);
        assert!(out.is_none());
    }

    // ─────────────────────────── exhaustive list coverage ───────────────────────────

    #[test]
    fn all_default_action_params_drop_url() {
        let cfg = cfg_on();
        for key in DEFAULT_ACTION_PARAMS.iter() {
            let url = format!("https://e.test/?{key}=1");
            assert!(
                filter_and_normalize_raw(&url, &cfg).is_none(),
                "expected {key} to drop the URL"
            );
        }
    }

    #[test]
    fn all_default_tracking_params_stripped_not_dropped() {
        let cfg = cfg_on();
        for key in DEFAULT_TRACKING_PARAMS.iter() {
            let url = format!("https://e.test/?{key}=1&p=1");
            let out = filter_and_normalize_raw(&url, &cfg)
                .unwrap_or_else(|| panic!("{key} must not drop the URL"));
            assert!(
                !out.contains(&format!("{key}=1")),
                "{key} not stripped: {out}"
            );
            assert!(out.contains("p=1"), "{key} run lost unrelated param: {out}");
        }
    }

    #[test]
    fn all_always_preserve_keys_survive_tier_b() {
        let cfg = cfg_on();
        for key in ALWAYS_PRESERVE.iter() {
            let url = format!("https://e.test/?{key}=1&utm_source=x");
            let out = filter_and_normalize_raw(&url, &cfg)
                .unwrap_or_else(|| panic!("{key} must not drop the URL"));
            assert!(
                out.contains(&format!("{key}=1")),
                "{key} was not preserved: {out}"
            );
            assert!(!out.contains("utm_source"), "{key} run: {out}");
        }
    }

    // ─────────────────────────── property tests (additional) ───────────────────────────

    proptest! {
        /// The function never panics on arbitrary printable-ASCII query
        /// strings, including ones that don't parse as valid percent-encoding.
        #[test]
        fn no_panic_on_arbitrary_query_text(q in "[-_a-zA-Z0-9=&%. ]{0,80}") {
            let cfg = cfg_on();
            let url = format!("https://example.com/path?{q}");
            let _ = filter_and_normalize_raw(&url, &cfg);
        }

        /// Varying the host as well as the query never panics.
        #[test]
        fn no_panic_on_arbitrary_host_and_query(
            host in "[a-z0-9]{1,10}\\.[a-z]{2,6}",
            pairs in prop::collection::vec(arb_pair(), 0..4)
        ) {
            let cfg = cfg_on();
            let q: String = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            let url = format!("https://{host}/path?{q}");
            let _ = filter_and_normalize_raw(&url, &cfg);
        }
    }

    // ─────────────────────────── HostPat::matches ───────────────────────────

    #[test]
    fn host_pat_any_matches_everything() {
        assert!(HostPat::Any.matches("anything.example.com"));
        assert!(HostPat::Any.matches(""));
    }

    #[test]
    fn host_pat_exact_matches_exact_host() {
        assert!(HostPat::Exact("youtube.com").matches("youtube.com"));
    }

    #[test]
    fn host_pat_exact_rejects_different_host() {
        assert!(!HostPat::Exact("youtube.com").matches("m.youtube.com"));
    }

    #[test]
    fn host_pat_exact_case_insensitive() {
        assert!(HostPat::Exact("youtube.com").matches("YouTube.COM"));
    }

    #[test]
    fn host_pat_suffix_matches_subdomain() {
        assert!(HostPat::Suffix(".youtube.com").matches("m.youtube.com"));
        assert!(HostPat::Suffix(".youtube.com").matches("www.youtube.com"));
    }

    #[test]
    fn host_pat_suffix_rejects_bare_domain_without_leading_dot_match() {
        // The pattern includes the leading '.', so the bare apex domain
        // (with nothing before it) does not match a Suffix pattern.
        assert!(!HostPat::Suffix(".youtube.com").matches("youtube.com"));
    }

    #[test]
    fn host_pat_suffix_rejects_unrelated_host() {
        assert!(!HostPat::Suffix(".youtube.com").matches("example.com"));
    }

    #[test]
    fn host_pat_suffix_rejects_lookalike_without_dot_boundary() {
        // "evilyoutube.com" ends with the same letters as ".youtube.com"
        // minus the dot; must not match without the literal '.' boundary.
        assert!(!HostPat::Suffix(".youtube.com").matches("evilyoutube.com"));
    }

    // ─────────────────────────── HostOverride::from_static ───────────────────────────

    #[test]
    fn host_override_from_static_canonicalizes_param_sets() {
        let entry = HostOverrideEntry {
            host_pat: HostPat::Any,
            when_path_contains: &["/foo/"],
            preserve_params: &["Add-To-Cart"],
            exempt_action_params: &["Remove-Item"],
            extra_tracking_params: &["My-Tracker"],
        };
        let ho = HostOverride::from_static(&entry);
        assert!(ho.preserve_params.contains("add_to_cart"));
        assert!(ho.exempt_action_params.contains("remove_item"));
        assert!(ho.extra_tracking_params.contains("my_tracker"));
    }

    #[test]
    fn host_override_from_static_leaves_path_substrings_untouched() {
        let entry = HostOverrideEntry {
            host_pat: HostPat::Any,
            when_path_contains: &["/Viewtopic.PHP"],
            preserve_params: &[],
            exempt_action_params: &[],
            extra_tracking_params: &[],
        };
        let ho = HostOverride::from_static(&entry);
        assert_eq!(ho.when_path_contains, vec!["/Viewtopic.PHP".to_string()]);
    }

    // ─────────────────────────── cross-tier interactions ───────────────────────────

    #[test]
    fn global_preserve_override_also_blocks_tier_a_drop() {
        // `eff_preserve` (which `with_overrides(preserve: ...)` feeds) is
        // checked first in the Tier A loop, before the action-param check —
        // so a preserved key survives even though it would otherwise drop
        // the whole URL.
        let cfg = cfg_on().with_overrides(RequestOverrides {
            preserve: Some(vec!["add_to_cart".to_string()]),
            ..Default::default()
        });
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=1", &cfg);
        assert!(
            out.is_some(),
            "preserved action param must not drop the URL"
        );
    }

    #[test]
    fn coarse_mode_alone_strips_action_param_without_dropping_url() {
        // drop_actions=false means Tier A never runs, but coarse mode still
        // strips the param (it is not in any preserve set) — the URL
        // survives with the param removed, it is not dropped outright.
        let mut cfg = cfg_on();
        cfg.drop_actions = false;
        cfg.coarse_strip_all = true;
        let out = filter_and_normalize_raw("https://e.test/?add-to-cart=1&p=1", &cfg).unwrap();
        assert!(!out.contains("add-to-cart"), "got {out}");
        assert!(out.contains("p=1"), "got {out}");
    }

    #[test]
    fn coarse_mode_honors_host_override_preserve() {
        // Coarse mode's `kept` filter checks `eff_preserve`, which is built
        // from host overrides too — a phpBB thread id survives coarse strip
        // even though it isn't in the global ALWAYS_PRESERVE set.
        let mut cfg = cfg_on();
        cfg.coarse_strip_all = true;
        let out = filter_and_normalize_raw(
            "https://forum.example.com/viewtopic.php?t=123&random=x",
            &cfg,
        )
        .unwrap();
        assert!(out.contains("t=123"), "got {out}");
        assert!(!out.contains("random"), "got {out}");
    }

    #[test]
    fn gov_host_coarse_mode_still_strips_action_param() {
        // On a .gov host Tier A is suppressed (drop_actions_active is
        // false), but coarse mode doesn't consult the gov exemption at
        // all — it strips any non-preserved param regardless of host.
        let mut cfg = cfg_on();
        cfg.coarse_strip_all = true;
        let out =
            filter_and_normalize_raw("https://senate.gov/?add-to-cart=1&docid=5", &cfg).unwrap();
        assert!(!out.contains("add-to-cart"), "got {out}");
        assert!(out.contains("docid=5"), "got {out}"); // ALWAYS_PRESERVE
    }

    #[test]
    fn off_mode_never_fails_on_unparseable_url() {
        let cfg = UrlFilterCfg::off();
        let out = filter_and_normalize_raw("not a url at all ???", &cfg);
        assert!(out.is_some());
    }

    #[test]
    fn coarse_strip_removes_question_mark_when_nothing_survives() {
        let mut cfg = cfg_on();
        cfg.coarse_strip_all = true;
        let out = filter_and_normalize_raw("https://e.test/blog?random=x&other=y", &cfg).unwrap();
        assert_eq!(out, "https://e.test/blog");
    }

    #[test]
    fn from_map_config_then_with_overrides_compose() {
        let raw = crw_core::config::MapUrlFilterConfig {
            strip_tracking_params: true,
            drop_action_urls: true,
            gov_tld_drop_actions: false,
            extra_tracking_params: vec!["from-config".to_string()],
            extra_action_params: vec![],
            extra_preserve_params: vec![],
        };
        let cfg = UrlFilterCfg::from_map_config(&raw).with_overrides(RequestOverrides {
            extra_tracking: Some(vec!["from-request".to_string()]),
            ..Default::default()
        });
        assert!(cfg.tracking_params.contains("from_config"));
        assert!(cfg.tracking_params.contains("from_request"));
    }

    #[test]
    fn canon_key_leading_and_trailing_hyphen() {
        assert_eq!(canon_key("-utm-"), "_utm_");
    }

    #[test]
    fn canon_key_consecutive_hyphens() {
        assert_eq!(canon_key("a--b"), "a__b");
    }

    #[test]
    fn gov_tld_suffixes_all_recognized() {
        for suf in GOV_TLD_SUFFIXES {
            // Dot-prefixed suffixes (".gov", ".mil") match as a subdomain
            // ("example.gov"); the bare suffix ("europa.eu") matches itself.
            let test_host = if suf.starts_with('.') {
                format!("example{suf}")
            } else {
                (*suf).to_string()
            };
            assert!(
                is_gov_host(&test_host),
                "expected {test_host} to be recognized via suffix {suf}"
            );
        }
    }

    #[test]
    fn all_default_action_params_case_insensitive_uppercase_variant() {
        let cfg = cfg_on();
        for key in DEFAULT_ACTION_PARAMS.iter() {
            let upper = key.to_uppercase();
            let url = format!("https://e.test/?{upper}=1");
            assert!(
                filter_and_normalize_raw(&url, &cfg).is_none(),
                "expected uppercase {upper} to drop the URL"
            );
        }
    }

    #[test]
    fn strip_tracking_off_leaves_tracking_param_when_action_off_too() {
        let cfg = cfg_on().with_overrides(RequestOverrides {
            strip_tracking: Some(false),
            drop_actions: Some(false),
            ..Default::default()
        });
        let out =
            filter_and_normalize_raw("https://e.test/?utm_source=x&add-to-cart=1", &cfg).unwrap();
        assert!(out.contains("utm_source=x"));
        assert!(out.contains("add-to-cart=1"));
    }

    #[test]
    fn request_overrides_default_is_all_none() {
        let ov = RequestOverrides::default();
        assert!(ov.strip_tracking.is_none());
        assert!(ov.drop_actions.is_none());
        assert!(ov.coarse_strip_all.is_none());
        assert!(ov.extra_tracking.is_none());
        assert!(ov.extra_action.is_none());
        assert!(ov.preserve.is_none());
    }
}
