//! RFC 8288 `Link` header parsing — Help Center answers with `<url>; rel="next"`.

use http::HeaderMap;

/// One `<url>; rel="…"` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub rel: String,
}

/// Parse one header value (several comma-separated links allowed).
#[must_use]
pub fn parse(value: &str) -> Vec<Link> {
    let mut out = Vec::new();
    for part in split_links(value) {
        let part = part.trim();
        let Some(rest) = part.strip_prefix('<') else {
            continue;
        };
        let Some((url, params)) = rest.split_once('>') else {
            continue;
        };
        let rel = params
            .split(';')
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("rel"))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        if !url.is_empty() {
            out.push(Link {
                url: url.to_string(),
                rel,
            });
        }
    }
    out
}

/// Commas inside `<…>` are part of the URL; split only on the ones between links.
fn split_links(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in value.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// The URL with `rel="<rel>"` across every `Link` header, if any.
#[must_use]
pub fn rel(headers: &HeaderMap, rel: &str) -> Option<String> {
    headers
        .get_all(http::header::LINK)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(parse)
        .find(|l| {
            l.rel
                .split_whitespace()
                .any(|r| r.eq_ignore_ascii_case(rel))
        })
        .map(|l| l.url)
}

/// `rel="next"`.
#[must_use]
pub fn next_url(headers: &HeaderMap) -> Option<String> {
    rel(headers, "next")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn parses_multiple_links_with_commas_in_urls() {
        let links = parse(
            r#"<https://x.zendesk.com/api/v2/help_center/articles.json?page=3&per_page=30&ids=1,2>; rel="next", <https://x.zendesk.com/api/v2/help_center/articles.json?page=1>; rel="prev""#,
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].rel, "next");
        assert!(links[0].url.ends_with("ids=1,2"));
        assert_eq!(links[1].rel, "prev");
        assert!(parse("garbage").is_empty());
        assert_eq!(parse("<https://a/b>")[0].rel, "");
    }

    #[test]
    fn next_url_reads_every_link_header() {
        let mut h = HeaderMap::new();
        h.append(
            http::header::LINK,
            HeaderValue::from_static("<https://a/prev>; rel=\"prev\""),
        );
        h.append(
            http::header::LINK,
            HeaderValue::from_static("<https://a/next>; rel=next"),
        );
        assert_eq!(next_url(&h).as_deref(), Some("https://a/next"));
        assert_eq!(rel(&h, "prev").as_deref(), Some("https://a/prev"));
        assert!(rel(&h, "last").is_none());
        assert!(next_url(&HeaderMap::new()).is_none());
    }
}
