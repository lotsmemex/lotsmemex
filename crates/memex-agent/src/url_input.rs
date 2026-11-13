use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_TEXT: usize = 16 * 1024;
pub const MAX_URL: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlSummary {
    pub host: String,
    pub path_class: String,
    pub query_present: bool,
}

pub fn extract_url(text: &str) -> Result<(String, UrlSummary)> {
    if text.len() > MAX_TEXT {
        bail!("input_too_large")
    }
    if text.chars().any(|c| c.is_control()) {
        bail!("input_contains_control")
    }
    let mut candidates = Vec::new();
    for (start, _) in text
        .match_indices("http://")
        .chain(text.match_indices("https://"))
    {
        let tail = &text[start..];
        let end = tail
            .char_indices()
            .find(|(_, c)| {
                c.is_whitespace()
                    || matches!(
                        c,
                        ',' | ';'
                            | '!'
                            | '"'
                            | '>'
                            | '，'
                            | '。'
                            | '；'
                            | '：'
                            | '！'
                            | '？'
                            | '、'
                            | '”'
                            | '’'
                            | '》'
                            | '】'
                            | '」'
                            | '』'
                    )
            })
            .map_or(tail.len(), |(i, _)| i);
        let mut raw = &tail[..end];
        raw = trim_punctuation(raw);
        if !raw.is_empty() {
            candidates.push(raw);
        }
    }
    if candidates.len() != 1 {
        bail!(if candidates.is_empty() {
            "url_missing"
        } else {
            "multiple_urls"
        })
    }
    let raw = candidates[0];
    if raw.len() > MAX_URL {
        bail!("url_too_large")
    }
    if raw.contains('\\') {
        bail!("url_malformed")
    }
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("url_malformed"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("url_malformed")
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("url_userinfo_forbidden")
    }
    if url.fragment().is_some() {
        bail!("url_fragment_forbidden")
    }
    let default = if url.scheme() == "http" { 80 } else { 443 };
    if url.port().is_some_and(|p| p != default) {
        bail!("url_port_forbidden")
    }
    let segments = url
        .path_segments()
        .map(|s| s.filter(|x| !x.is_empty()).count())
        .unwrap_or(0);
    let path_class = match segments {
        0 => "root",
        1 => "single_segment",
        _ => "nested",
    }
    .into();
    Ok((
        url.to_string(),
        UrlSummary {
            host: url.host_str().unwrap().to_ascii_lowercase(),
            path_class,
            query_present: url.query().is_some(),
        },
    ))
}

fn trim_punctuation(mut s: &str) -> &str {
    while let Some(c) = s.chars().last() {
        let trim = matches!(
            c,
            '.' | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
                | '、'
                | '”'
                | '’'
                | '》'
                | '】'
                | '」'
                | '』'
        ) || (c == ')' && s.matches(')').count() > s.matches('(').count())
            || (c == ']' && s.matches(']').count() > s.matches('[').count());
        if !trim {
            break;
        }
        s = &s[..s.len() - c.len_utf8()];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matrix() {
        assert_eq!(
            extract_url("Read this https://example.test/a/b?q=x, please.")
                .unwrap()
                .1
                .path_class,
            "nested"
        );
        assert_eq!(
            extract_url("请阅读：https://例子.test/文章。谢谢")
                .unwrap()
                .1
                .path_class,
            "single_segment"
        );
        for (s, e) in [
            ("nothing", "url_missing"),
            ("https://a.test https://b.test", "multiple_urls"),
            ("https://a.test and https://a.test", "multiple_urls"),
            ("https://u:p@a.test/", "url_userinfo_forbidden"),
            ("https://a.test/\n", "input_contains_control"),
        ] {
            assert_eq!(extract_url(s).unwrap_err().to_string(), e);
        }
        assert_eq!(
            extract_url(&format!("https://a.test/{}", "x".repeat(MAX_URL)))
                .unwrap_err()
                .to_string(),
            "url_too_large"
        );
        assert!(extract_url("See https://a.test/wiki/Foo_(bar). now")
            .unwrap()
            .0
            .contains("(bar)"));
        assert_eq!(
            extract_url("Read <https://a.test/quoted> now").unwrap().0,
            "https://a.test/quoted"
        );
    }
}
