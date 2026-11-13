//! Markdown links, shared by the reader's own renderer and the sanitizer run
//! before any Markdown reaches a `TextView` (AI 纪要, chat replies).
//!
//! Only `http` / `https` targets are ever clickable; anything else (javascript:,
//! file:, data:, relative paths) is shown as its text. Images are never loaded
//! from the Markdown: they become a labelled link (http/https) or a label.

use std::ops::Range;

/// A `[text](url)` or `![alt](url)` starting at a given byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub image: bool,
    /// The bytes of the link text (or alt) in the source.
    pub text: Range<usize>,
    /// The target as written (angle brackets and title removed).
    pub url: String,
    /// Byte after the closing `)`.
    pub end: usize,
}

/// The target if it may be opened: `http(s)://` with a host, no spaces or controls.
pub fn safe_url(url: &str) -> Option<&str> {
    let url = url.trim();
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    let host_ok = rest.split(['/', '?', '#']).next().is_some_and(|h| !h.is_empty());
    (host_ok && !url.chars().any(|c| c.is_whitespace() || c.is_control())).then_some(url)
}

/// Parses a link or image at byte `i` (pointing at `[` or `!`).
pub fn link_at(s: &str, i: usize) -> Option<Link> {
    let b = s.as_bytes();
    let image = b.get(i) == Some(&b'!');
    let open = if image { i + 1 } else { i };
    if b.get(open) != Some(&b'[') {
        return None;
    }
    // Matching `]`, brackets balanced, escapes skipped, within one paragraph.
    let mut depth = 0usize;
    let mut j = open + 1;
    let close = loop {
        match *b.get(j)? {
            b'\\' => j += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => break j,
            b']' => depth -= 1,
            b'\n' if b.get(j + 1) == Some(&b'\n') => return None,
            _ => {}
        }
        j += 1;
    };
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let mut k = close + 2;
    while b.get(k) == Some(&b' ') {
        k += 1;
    }
    let (url, mut k) = if b.get(k) == Some(&b'<') {
        let end = k + 1 + s.get(k + 1..)?.find(['>', '\n'])?;
        (b.get(end) == Some(&b'>')).then_some(())?;
        (s[k + 1..end].to_string(), end + 1)
    } else {
        let start = k;
        let mut parens = 0usize;
        while let Some(&c) = b.get(k) {
            match c {
                b'(' => parens += 1,
                b')' if parens == 0 => break,
                b')' => parens -= 1,
                c if c.is_ascii_whitespace() => break,
                _ => {}
            }
            k += 1;
        }
        (s[start..k].to_string(), k)
    };
    while b.get(k) == Some(&b' ') {
        k += 1;
    }
    // Optional title: "…", '…' or (…).
    if let Some(&q) = b.get(k).filter(|c| matches!(c, b'"' | b'\'' | b'(')) {
        let end = if q == b'(' { b')' } else { q };
        let len = s.get(k + 1..)?.find(end as char)?;
        k += 1 + len + 1;
        while b.get(k) == Some(&b' ') {
            k += 1;
        }
    }
    (b.get(k) == Some(&b')')).then(|| Link { image, text: open + 1..close, url, end: k + 1 })
}

/// Label of an image as a link (the reader shows the same words).
pub fn image_label(alt: &str) -> String {
    let alt = alt.trim();
    if alt.is_empty() { crate::tx("图片").into() } else { crate::txf("图片：{0}", &[&alt]) }
}

fn escape_label(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

/// Markdown safe to hand to a renderer that follows links and loads images:
/// outside code, images become `[图片：alt](url)` (or the label alone), links
/// with a target that is not http/https become their text, autolinks and
/// reference definitions with such targets are dropped to text.
pub fn safe_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut fence: Option<&str> = None;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(f) = fence {
            out.push_str(line);
            if trimmed.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            out.push_str(line);
            continue;
        }
        // `[id]: target` reference definitions: unsafe targets are not kept.
        if let Some(rest) = trimmed.strip_prefix('[').and_then(|r| r.split_once("]:")) {
            if !rest.0.contains(']') && safe_url(rest.1.trim().trim_start_matches('<').split(['>', ' ']).next().unwrap_or("")).is_none() {
                out.push_str(&escape_label(line));
                continue;
            }
        }
        out.push_str(&safe_inline(line));
    }
    out
}

fn safe_inline(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        match b[i] {
            b'\\' if i + 1 < line.len() => {
                let n = line[i + 1..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&line[i..i + 1 + n]);
                i += 1 + n;
                continue;
            }
            b'`' => {
                let ticks = line[i..].bytes().take_while(|c| *c == b'`').count();
                let delim = &line[i..i + ticks];
                if let Some(len) = line[i + ticks..].find(delim) {
                    let end = i + ticks + len + ticks;
                    out.push_str(&line[i..end]);
                    i = end;
                    continue;
                }
            }
            b'!' | b'[' => {
                if let Some(l) = link_at(line, i) {
                    let text = safe_inline(&line[l.text.clone()]);
                    match (l.image, safe_url(&l.url)) {
                        (true, Some(u)) => out.push_str(&format!("[{}](<{u}>)", escape_label(&image_label(&line[l.text.clone()])))),
                        (true, None) => out.push_str(&format!("〔{}〕", escape_label(&image_label(&line[l.text.clone()])))),
                        (false, Some(u)) => out.push_str(&format!("[{text}](<{u}>)")),
                        (false, None) => out.push_str(&text),
                    }
                    i = l.end;
                    continue;
                }
            }
            b'<' => {
                // Autolink `<scheme:…>`: kept only for http(s).
                if let Some(len) = line[i + 1..].find('>') {
                    let inner = &line[i + 1..i + 1 + len];
                    if inner.contains(':') && !inner.contains(char::is_whitespace) && !inner.starts_with('/') {
                        match safe_url(inner) {
                            Some(_) => out.push_str(&line[i..i + len + 2]),
                            None => out.push_str(inner),
                        }
                        i += len + 2;
                        continue;
                    }
                }
            }
            _ => {}
        }
        let ch = line[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_parse_with_titles_brackets_and_parens() {
        let s = "看 [YouTube: https://youtu.be/x](https://youtu.be/x \"标题\") 完";
        let l = link_at(s, s.find('[').unwrap()).unwrap();
        assert_eq!(&s[l.text.clone()], "YouTube: https://youtu.be/x");
        assert_eq!(l.url, "https://youtu.be/x");
        assert_eq!(&s[l.end..], " 完");
        let w = "[维基](https://en.wikipedia.org/wiki/Rust_(language))。";
        assert_eq!(link_at(w, 0).unwrap().url, "https://en.wikipedia.org/wiki/Rust_(language)");
        assert_eq!(link_at("[a [b] c](<https://x.example/a b>)", 0).unwrap().url, "https://x.example/a b");
        assert!(link_at("[没有目标]", 0).is_none());
        assert!(link_at("[a] (https://x.example)", 0).is_none(), "a space before ( is not a link");
        let img = link_at("![封面](https://x.example/c.png)", 0).unwrap();
        assert!(img.image);
    }

    #[test]
    fn only_http_and_https_are_safe() {
        assert_eq!(safe_url(" https://36kr.com/p/1 "), Some("https://36kr.com/p/1"));
        assert_eq!(safe_url("HTTP://Example.org"), Some("HTTP://Example.org"));
        for bad in ["javascript:alert(1)", "JavaScript:alert(1)", "file:///etc/passwd", "data:text/html,x", "/relative", "https://", "https:// space.example", "mailto:a@b.c"] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_sanitizer_keeps_safe_links_drops_unsafe_ones_and_never_loads_images() {
        let md = "[好](https://a.example) [坏](javascript:alert(1)) ![图](https://a.example/i.png) ![本地](file:///x.png) <javascript:x> <https://b.example>\n`[x](javascript:y)` 保持原样\n```\n[z](javascript:z)\n```\n[ref]: javascript:evil\n";
        let out = safe_markdown(md);
        assert!(out.contains("[好](<https://a.example>)"));
        assert!(!out.contains("javascript:alert"), "{out}");
        assert!(out.contains(" 坏 "));
        assert!(out.contains("[图片：图](<https://a.example/i.png>)"));
        assert!(out.contains("〔图片：本地〕") && !out.contains("file:"));
        assert!(!out.contains("!["), "no image survives: {out}");
        assert!(out.contains(" javascript:x ") && out.contains("<https://b.example>"));
        assert!(out.contains("`[x](javascript:y)` 保持原样"), "code spans are literal");
        assert!(out.contains("```\n[z](javascript:z)\n```"), "fenced code is literal");
        assert!(out.contains("\\[ref\\]: javascript:evil"), "an unsafe reference definition is text");
    }
}
