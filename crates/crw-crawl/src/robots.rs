use crw_core::error::{CrwError, CrwResult};

/// A single robots.txt rule (Allow or Disallow).
#[derive(Debug, Clone)]
struct Rule {
    pattern: String,
    allow: bool,
}

/// Simple robots.txt parser with wildcard and Allow support.
#[derive(Debug, Clone)]
pub struct RobotsTxt {
    rules: Vec<Rule>,
    pub sitemaps: Vec<String>,
}

impl RobotsTxt {
    pub async fn fetch(base_url: &str, client: &reqwest::Client) -> CrwResult<Self> {
        let url = format!("{}/robots.txt", base_url.trim_end_matches('/'));

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| CrwError::HttpError(crw_core::error::reqwest_message(e)))?;

        if !resp.status().is_success() {
            return Ok(Self {
                rules: vec![],
                sitemaps: vec![],
            });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| CrwError::HttpError(crw_core::error::reqwest_message(e)))?;

        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        let mut sitemaps = Vec::new();
        let mut in_our_section = false;
        // Whether the previous directive line was also a `User-agent:`.
        //
        // Per RFC 9309 §2.2.1 a run of consecutive `User-agent:` lines heads
        // ONE group, and the group applies if ANY of them matches. Reading
        // `in_our_section` from the last line alone silently discarded the
        // rules of the extremely common shape
        //
        //     User-agent: *
        //     User-agent: BadBot
        //     Disallow: /
        //
        // because the last agent is not us. A site-wide `Disallow` that
        // named us via `*` was dropped and we crawled what it forbade. That is
        // the fail-open direction, which is the expensive one here.
        let mut in_agent_run = false;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(agent) = directive_value(line, "user-agent:") {
                if !in_agent_run {
                    // First agent line after a rule: a new group begins.
                    in_our_section = false;
                    in_agent_run = true;
                }
                let agent = agent.to_ascii_lowercase();
                if agent == "*" || agent.contains("crw") {
                    in_our_section = true;
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

            if in_our_section {
                if let Some(path) = directive_value(line, "disallow:") {
                    if !path.is_empty() {
                        rules.push(Rule {
                            pattern: path.to_string(),
                            allow: false,
                        });
                    }
                } else if let Some(path) = directive_value(line, "allow:")
                    && !path.is_empty()
                {
                    rules.push(Rule {
                        pattern: path.to_string(),
                        allow: true,
                    });
                }
            }
        }

        Self { rules, sitemaps }
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

/// Safely extract the value after a directive prefix (case-insensitive match).
///
/// Compares the head of `line` in place rather than lowercasing it first. The
/// previous version validated `prefix.len()` against `line.to_lowercase()` and
/// then applied that offset to `line`, but `to_lowercase` is full Unicode and
/// does not preserve byte length (`İ` U+0130 is 2 bytes and lowercases to 3;
/// `K` U+212A is 3 bytes and lowercases to 1). The offset could therefore land
/// past the end of `line`, or mid-character, and panic on remote
/// attacker-controlled `robots.txt`.
///
/// No live panic was reachable, because none of the four prefixes in use
/// contains a letter with a length-changing uppercase mapping, but it became
/// a remote panic the day someone added a prefix containing `k`. Comparing
/// with `eq_ignore_ascii_case` is byte-length-preserving by construction (all
/// four prefixes are ASCII), and `get` returns `None` rather than panicking if
/// the split would land mid-character.
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

    /// `directive_value` used to validate the prefix length against a
    /// lowercased copy and then slice the original at that offset.
    /// `to_lowercase` is full Unicode and does not preserve byte length, so a
    /// line whose head changes length under lowercasing could slice out of
    /// bounds or mid-character. Feed it lines that would have tripped that.
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
        // And the whole parser over the same corpus.
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
}
