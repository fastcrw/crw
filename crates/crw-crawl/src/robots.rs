use crw_core::error::{CrwError, CrwResult};

/// A single robots.txt rule (Allow or Disallow).
#[derive(Debug, Clone)]
struct Rule {
    pattern: String,
    allow: bool,
}

const ROBOTS_PRODUCT_TOKEN: &str = "crw";
const MAX_ROBOTS_BYTES: usize = 500 * 1024;

/// Simple robots.txt parser with wildcard and Allow support.
#[derive(Debug, Clone, Default)]
pub struct RobotsTxt {
    rules: Vec<Rule>,
    pub sitemaps: Vec<String>,
}

impl RobotsTxt {
    /// Fetch and parse an origin's `/robots.txt`.
    ///
    /// A 4xx is a site that simply has no robots.txt, which is no rules rather
    /// than a problem, so it comes back as an empty rule set. A 5xx or a
    /// transport failure comes back as an error, but only so the caller can log
    /// which origin went dark: every caller proceeds with no rules either way.
    /// Failing a job on an unreadable robots.txt would hand any origin that
    /// 503s the file a way to stop a crawl outright, and the pages it never
    /// asked us to withhold would go unfetched.
    pub async fn fetch(base_url: &str, client: &reqwest::Client) -> CrwResult<Self> {
        let url = format!("{}/robots.txt", base_url.trim_end_matches('/'));

        let resp = client.get(&url).send().await.map_err(|e| {
            CrwError::TargetUnreachable(format!(
                "robots.txt request failed: {}",
                crw_core::error::reqwest_message(e)
            ))
        })?;

        if resp.status().is_client_error() {
            return Ok(Self::default());
        }
        if !resp.status().is_success() {
            return Err(CrwError::TargetUnreachable(format!(
                "robots.txt returned HTTP {}",
                resp.status().as_u16()
            )));
        }

        let (mut bytes, truncated) =
            crw_core::body::read_capped(resp.bytes_stream(), MAX_ROBOTS_BYTES)
                .await
                .map_err(|e| {
                    CrwError::TargetUnreachable(format!(
                        "robots.txt body failed: {}",
                        crw_core::error::reqwest_message(e)
                    ))
                })?;
        // Only a genuine truncation gets here, so this drops a half-read final
        // line rather than the last rule of a file that landed on the cap.
        if truncated {
            let end = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1);
            bytes.truncate(end);
        }
        let text = String::from_utf8_lossy(&bytes);
        Ok(Self::parse(&text))
    }

    /// Parse into the rules that bind us.
    ///
    /// Per RFC 9309 §2.2.1 a run of consecutive `User-agent:` lines heads ONE
    /// group and the group applies if ANY of them matches, so the agents are
    /// accumulated over the run rather than read off its last line. Per §2.2.1
    /// a group naming our product token exactly also SUPPRESSES the `*` group,
    /// which is why the two are collected separately and chosen between at the
    /// end rather than merged as they are read.
    pub fn parse(text: &str) -> Self {
        let mut exact: Vec<Rule> = Vec::new();
        let mut wildcard: Vec<Rule> = Vec::new();
        let mut sitemaps = Vec::new();
        // Which bucket the group currently being read writes into, and whether
        // any exact group has been seen at all. An exact group with no rules
        // still suppresses `*`, so this cannot be inferred from `exact`.
        let (mut group_exact, mut group_wildcard, mut saw_exact) = (false, false, false);
        let mut in_agent_run = false;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(agent) = directive_value(line, "user-agent:") {
                if !in_agent_run {
                    (group_exact, group_wildcard) = (false, false);
                    in_agent_run = true;
                }
                if agent.eq_ignore_ascii_case(ROBOTS_PRODUCT_TOKEN) {
                    (group_exact, saw_exact) = (true, true);
                } else if agent == "*" {
                    group_wildcard = true;
                }
                continue;
            }

            // `Sitemap:` is a non-group record (RFC 9309 §2.2.3) and may sit
            // anywhere, including inside a run of agent lines, so it must not
            // close the run.
            if let Some(url) = directive_value(line, "sitemap:") {
                if !url.is_empty() {
                    sitemaps.push(url.to_string());
                }
                continue;
            }

            // Any group-member directive closes the agent run.
            in_agent_run = false;

            let rule = if let Some(pattern) = directive_value(line, "disallow:") {
                Rule {
                    pattern: pattern.to_string(),
                    allow: false,
                }
            } else if let Some(pattern) = directive_value(line, "allow:") {
                Rule {
                    pattern: pattern.to_string(),
                    allow: true,
                }
            } else {
                continue;
            };
            if rule.pattern.is_empty() {
                continue;
            }
            // A group naming us exactly wins outright, so a group that matches
            // both writes only to `exact`.
            if group_exact {
                exact.push(rule);
            } else if group_wildcard {
                wildcard.push(rule);
            }
        }

        Self {
            rules: if saw_exact { exact } else { wildcard },
            sitemaps,
        }
    }

    /// Check if a path is allowed using specificity-based matching.
    /// Per RFC 9309: the most specific (longest) pattern wins.
    /// Wildcard `*` and anchor `$` characters are excluded from length calculation.
    /// If equal effective length, Allow wins over Disallow.
    pub fn is_allowed(&self, path: &str) -> bool {
        let mut best_match: Option<&Rule> = None;
        let mut best_len: usize = 0;

        for rule in &self.rules {
            if matches_pattern(path, &rule.pattern) {
                let len = effective_pattern_len(&rule.pattern);
                if len > best_len || (len == best_len && rule.allow) {
                    best_len = len;
                    best_match = Some(rule);
                }
            }
        }

        match best_match {
            Some(rule) => rule.allow,
            None => true, // No matching rule means allowed
        }
    }

    /// Check whether a URL may be fetched, matching on **path *and* query**.
    ///
    /// Prefer this over calling [`Self::is_allowed`] with `url.path()`: robots
    /// patterns routinely key on the query string, and dropping it silently
    /// allows everything the site tried to forbid. Hacker News, for example,
    /// disallows `/hide?`, `/vote?` and `/reply?` — matching bare `/hide`
    /// against pattern `/hide?` fails, so `/hide?id=1&goto=news` would be
    /// fetched despite being explicitly forbidden.
    pub fn is_url_allowed(&self, url: &url::Url) -> bool {
        let path_and_query = match url.query() {
            Some(q) => format!("{}?{}", url.path(), q),
            None => url.path().to_string(),
        };
        self.is_allowed(&path_and_query)
    }
}

/// Effective pattern length for specificity calculation.
/// Excludes wildcard `*` and end-anchor `$` characters per RFC 9309.
fn effective_pattern_len(pattern: &str) -> usize {
    pattern.chars().filter(|&c| c != '*' && c != '$').count()
}

/// Simple glob matching for robots.txt patterns.
/// Supports `*` (any sequence of characters) and `$` (end of string).
fn matches_pattern(path: &str, pattern: &str) -> bool {
    let anchored_end = pattern.ends_with('$');
    let pattern = if anchored_end {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };

    if !pattern.contains('*') {
        // Simple prefix match
        if anchored_end {
            path == pattern
        } else {
            path.starts_with(pattern)
        }
    } else {
        // Split by * and match segments in order
        let segments: Vec<&str> = pattern.split('*').collect();
        let mut pos = 0;

        for (i, segment) in segments.iter().enumerate() {
            if segment.is_empty() {
                continue;
            }
            if i == 0 {
                // First segment must match at start
                if !path[pos..].starts_with(segment) {
                    return false;
                }
                pos += segment.len();
            } else {
                // Subsequent segments can match anywhere after current position
                match path[pos..].find(segment) {
                    Some(idx) => pos += idx + segment.len(),
                    None => return false,
                }
            }
        }

        if anchored_end {
            pos == path.len()
        } else {
            true
        }
    }
}

/// Extract a directive value with an ASCII case-insensitive prefix.
fn directive_value<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let value = line[prefix.len()..].trim();
    // Strip inline comments (e.g. "Disallow: /admin # admin panel")
    let value = value.split('#').next().unwrap_or(value).trim();
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_group_suppresses_wildcard_rules() {
        let robots = RobotsTxt::parse(
            "User-agent: *\nAllow: /private\nUser-agent: crw\nDisallow: /private\n",
        );
        assert!(!robots.is_allowed("/private"));

        let robots =
            RobotsTxt::parse("User-agent: *\nDisallow: /public\nUser-agent: CRW\nAllow: /public\n");
        assert!(robots.is_allowed("/public"));
    }

    #[test]
    fn multiple_exact_groups_are_combined() {
        let robots =
            RobotsTxt::parse("User-agent: crw\nDisallow: /one\nUser-agent: CRW\nDisallow: /two\n");
        assert!(!robots.is_allowed("/one"));
        assert!(!robots.is_allowed("/two"));
    }

    #[test]
    fn substring_agent_does_not_match_crw() {
        let robots =
            RobotsTxt::parse("User-agent: *\nAllow: /\nUser-agent: notcrwbot\nDisallow: /\n");
        assert!(robots.is_allowed("/page"));
    }

    #[test]
    fn empty_exact_group_still_suppresses_wildcard_rules() {
        let robots = RobotsTxt::parse(
            "User-agent: *\nDisallow: /private\nUser-agent: crw\nSitemap: https://e.test/map.xml\n",
        );
        assert!(robots.is_allowed("/private"));
    }

    #[test]
    fn directives_before_the_first_group_are_ignored() {
        let robots = RobotsTxt::parse("Disallow: /\nUser-agent: *\nAllow: /\n");
        assert!(robots.is_allowed("/page"));
    }

    /// RFC 9309 §2.2.1: a run of consecutive `User-agent:` lines heads one
    /// group, and the group applies if ANY of them matches. Reading only the
    /// last line dropped this site-wide `Disallow`, the fail-open direction.
    #[test]
    fn consecutive_user_agent_lines_form_one_group() {
        let r = RobotsTxt::parse("User-agent: *\nUser-agent: BadBot\nDisallow: /\n");
        assert!(!r.is_allowed("/anything"), "the `*` group was discarded");
    }

    /// The same shape with our own name last must also apply.
    #[test]
    fn consecutive_user_agent_lines_match_on_any_member() {
        let r = RobotsTxt::parse("User-agent: SomeBot\nUser-agent: crw\nDisallow: /private\n");
        assert!(!r.is_allowed("/private/x"));
        assert!(r.is_allowed("/public"));
    }

    /// A rule line closes the run, so the next `User-agent:` starts a fresh
    /// group and must NOT inherit the previous group's match.
    #[test]
    fn a_rule_line_closes_the_agent_run() {
        let r = RobotsTxt::parse(
            "User-agent: *\nDisallow: /everyone\nUser-agent: OtherBot\nDisallow: /theirs\n",
        );
        assert!(!r.is_allowed("/everyone"), "our own group still applies");
        assert!(
            r.is_allowed("/theirs"),
            "a group naming only OtherBot must not bind us"
        );
    }

    /// `Sitemap:` is a non-group record and may appear anywhere, including
    /// between agent lines, so it must not close the run.
    #[test]
    fn a_sitemap_line_does_not_close_the_agent_run() {
        let r = RobotsTxt::parse(
            "User-agent: *\nSitemap: https://e.test/sitemap.xml\nUser-agent: BadBot\nDisallow: /\n",
        );
        assert_eq!(r.sitemaps, vec!["https://e.test/sitemap.xml".to_string()]);
        assert!(!r.is_allowed("/anything"));
    }

    #[test]
    fn directive_parsing_does_not_panic_on_length_changing_unicode() {
        for line in [
            "İser-agent: *",
            "\u{212A}isallow: /x",
            "Ｕser-agent: *",
            "é",
            "",
            ":",
            "user-agent",
        ] {
            let _ = directive_value(line, "user-agent:");
            let _ = directive_value(line, "disallow:");
            let _ = directive_value(line, "sitemap:");
        }
        let r =
            RobotsTxt::parse("İser-agent: *\n\u{212A}isallow: /x\nUser-agent: *\nDisallow: /y\n");
        assert!(!r.is_allowed("/y"));
    }

    /// Directive prefixes stay case-insensitive after the rewrite.
    #[test]
    fn directive_prefixes_remain_case_insensitive() {
        let r =
            RobotsTxt::parse("USER-AGENT: *\nDISALLOW: /admin\nSITEMAP: https://e.test/s.xml\n");
        assert!(!r.is_allowed("/admin/x"));
        assert_eq!(r.sitemaps, vec!["https://e.test/s.xml".to_string()]);
    }

    #[test]
    fn parses_robots_txt() {
        let text = r#"
User-agent: *
Disallow: /admin/
Disallow: /private/

Sitemap: https://example.com/sitemap.xml
"#;
        let robots = RobotsTxt::parse(text);
        assert!(!robots.is_allowed("/admin/page"));
        assert!(robots.is_allowed("/public/page"));
        assert_eq!(robots.sitemaps, vec!["https://example.com/sitemap.xml"]);
    }

    #[test]
    fn handles_edge_cases() {
        let text = "User-agent:\nDisallow:\nSitemap:\n";
        let robots = RobotsTxt::parse(text);
        assert!(robots.is_allowed("/anything"));
        assert!(robots.sitemaps.is_empty());
    }

    #[test]
    fn wildcard_pattern_matching() {
        let text = "User-agent: *\nDisallow: /*.pdf\n";
        let robots = RobotsTxt::parse(text);
        assert!(!robots.is_allowed("/document.pdf"));
        assert!(!robots.is_allowed("/path/to/file.pdf"));
        assert!(robots.is_allowed("/document.html"));
    }

    #[test]
    fn dollar_end_anchor() {
        let text = "User-agent: *\nDisallow: /*.pdf$\n";
        let robots = RobotsTxt::parse(text);
        assert!(!robots.is_allowed("/document.pdf"));
        assert!(robots.is_allowed("/document.pdf?query=1"));
    }

    #[test]
    fn allow_overrides_disallow() {
        let text = r#"
User-agent: *
Disallow: /private/
Allow: /private/public-page
"#;
        let robots = RobotsTxt::parse(text);
        assert!(!robots.is_allowed("/private/secret"));
        assert!(robots.is_allowed("/private/public-page"));
    }

    #[test]
    fn specificity_longer_pattern_wins() {
        let text = r#"
User-agent: *
Disallow: /
Allow: /public/
"#;
        let robots = RobotsTxt::parse(text);
        assert!(!robots.is_allowed("/private"));
        assert!(robots.is_allowed("/public/page"));
    }

    #[test]
    fn equal_length_allow_wins() {
        let text = r#"
User-agent: *
Disallow: /path
Allow: /path
"#;
        let robots = RobotsTxt::parse(text);
        assert!(robots.is_allowed("/path"));
    }

    #[tokio::test]
    /// A site with no robots.txt forbids nothing; an origin that cannot serve
    /// the file reports an error the caller logs and then ignores. Both end up
    /// crawling, which is what the callers assert.
    async fn fetch_classifies_unavailable_and_unreachable_statuses() {
        let unavailable = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/robots.txt"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&unavailable)
            .await;
        let absent = RobotsTxt::fetch(&unavailable.uri(), &reqwest::Client::new())
            .await
            .expect("a missing robots.txt is not an error");
        assert!(absent.is_allowed("/anything"), "no file means no rules");

        let unreachable = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/robots.txt"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&unreachable)
            .await;
        assert!(matches!(
            RobotsTxt::fetch(&unreachable.uri(), &reqwest::Client::new()).await,
            Err(CrwError::TargetUnreachable(_))
        ));

        let slow = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/robots.txt"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(200)),
            )
            .mount(&slow)
            .await;
        let short_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(10))
            .build()
            .unwrap();
        assert!(matches!(
            RobotsTxt::fetch(&slow.uri(), &short_client).await,
            Err(CrwError::TargetUnreachable(_))
        ));
    }

    #[tokio::test]
    async fn fetch_caps_the_decoded_gzip_body() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let mut text = "User-agent: crw\nDisallow: /blocked\n".to_string();
        text.push_str(&"x".repeat(MAX_ROBOTS_BYTES - text.len()));
        text.push_str("\nAllow: /blocked\n");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/robots.txt"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .set_body_bytes(compressed),
            )
            .mount(&server)
            .await;

        let robots = RobotsTxt::fetch(&server.uri(), &reqwest::Client::new())
            .await
            .unwrap();
        assert!(!robots.is_allowed("/blocked"));
    }
}
