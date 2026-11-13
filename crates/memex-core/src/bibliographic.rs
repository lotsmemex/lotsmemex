//! Bibliographic metadata v1 (draft2): identifier normalization and partial
//! dates. Pure, offline functions; no registry lookup is implied by `valid`.

use crate::{Creator, IdentifierKind, ItemIdentifier};
use serde::{Deserialize, Serialize};

pub const IDENTIFIER_NORMALIZER_VERSION: &str = "memex.identifier_normalizer.v1";

pub const QUERY_DATE_POLICY: &str = "publication_or_capture_v1";

/// Verified read projection, never a replacement for stored publication evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectiveDates {
    pub effective_date: Option<PartialDate>,
    pub effective_date_basis: EffectiveDateBasis,
    pub effective_capture_at: Option<String>,
    pub effective_capture_origin: Option<EffectiveCaptureOrigin>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveDateBasis {
    Published,
    InheritedFromParent,
    Captured,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveCaptureOrigin {
    NativeCapture,
    EvernoteClip,
    ZoteroDateAdded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Valid,
    Invalid,
    Unparsed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifierValidation {
    pub status: ValidationStatus,
    pub reason: Option<String>,
    pub normalizer_version: String,
}

/// Server-computed, read-only view of one identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedIdentifier {
    pub kind: IdentifierKind,
    /// Normalized stored/display value; `None` unless `validation.status` is valid
    /// (PMID is retained trimmed with status unparsed).
    pub value: Option<String>,
    /// Exact duplicate key (DOI or ISBN-13 only). ISSN/PMID never have one.
    pub comparison_key: Option<String>,
    pub validation: IdentifierValidation,
}

type Check = Result<String, (ValidationStatus, &'static str)>;

fn invalid(reason: &'static str) -> (ValidationStatus, &'static str) {
    (ValidationStatus::Invalid, reason)
}

pub fn normalize_identifier(kind: IdentifierKind, raw: &str) -> NormalizedIdentifier {
    let checked = match kind {
        IdentifierKind::Doi => normalize_doi(raw),
        IdentifierKind::Isbn => normalize_isbn(raw),
        IdentifierKind::Issn => normalize_issn(raw),
        IdentifierKind::Pmid => Err((ValidationStatus::Unparsed, "not_validated")),
    };
    let (value, comparison_key, status, reason) = match checked {
        Ok(value) => {
            let key =
                matches!(kind, IdentifierKind::Doi | IdentifierKind::Isbn).then(|| value.clone());
            (Some(value), key, ValidationStatus::Valid, None)
        }
        Err((status, reason)) => {
            let retained = (kind == IdentifierKind::Pmid).then(|| raw.trim().to_owned());
            (retained, None, status, Some(reason.to_owned()))
        }
    };
    NormalizedIdentifier {
        kind,
        value,
        comparison_key,
        validation: IdentifierValidation {
            status,
            reason,
            normalizer_version: IDENTIFIER_NORMALIZER_VERSION.to_owned(),
        },
    }
}

impl ItemIdentifier {
    pub fn normalized(&self) -> NormalizedIdentifier {
        normalize_identifier(self.kind, &self.value)
    }
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    (value.len() >= prefix.len()
        && value.is_char_boundary(prefix.len())
        && value[..prefix.len()].eq_ignore_ascii_case(prefix))
    .then(|| &value[prefix.len()..])
}

fn percent_decode_once(value: &str) -> Check {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .filter(|_| {
                    bytes[index + 1].is_ascii_hexdigit() && bytes[index + 2].is_ascii_hexdigit()
                })
                .ok_or(invalid("bad_percent_encoding"))?;
            out.push(hex);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| invalid("bad_percent_encoding"))
}

pub fn normalize_doi(raw: &str) -> Check {
    let trimmed = raw.trim();
    let bare = if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "doi:") {
        rest.trim_start().to_owned()
    } else if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "https://")
        .or_else(|| strip_prefix_ignore_ascii_case(trimmed, "http://"))
    {
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.contains('@') {
            return Err(invalid("url_credentials"));
        }
        if !authority.eq_ignore_ascii_case("doi.org")
            && !authority.eq_ignore_ascii_case("dx.doi.org")
        {
            return Err(invalid("not_doi_resolver"));
        }
        let path = &rest[authority_end..];
        if path.contains(['?', '#']) {
            return Err(invalid("url_query_or_fragment"));
        }
        let path = path.strip_prefix('/').ok_or(invalid("missing_doi"))?;
        percent_decode_once(path)?
    } else {
        trimmed.to_owned()
    };
    if bare.is_empty() {
        return Err(invalid("empty"));
    }
    if bare.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid("whitespace_or_control"));
    }
    let (prefix, suffix) = bare.split_once('/').ok_or(invalid("missing_suffix"))?;
    let registrant = strip_prefix_ignore_ascii_case(prefix, "10.").ok_or(invalid("prefix"))?;
    if registrant
        .split('.')
        .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid("prefix"));
    }
    if suffix.is_empty() {
        return Err(invalid("missing_suffix"));
    }
    if suffix.chars().any(|c| {
        !c.is_ascii()
            && (c.to_lowercase().ne(std::iter::once(c)) || c.to_uppercase().ne(std::iter::once(c)))
    }) {
        return Err((ValidationStatus::Unparsed, "non_ascii_case"));
    }
    Ok(format!("{prefix}/{}", suffix.to_ascii_lowercase()))
}

fn strip_label<'a>(value: &'a str, labels: &[&str]) -> &'a str {
    for label in labels {
        if let Some(rest) = strip_prefix_ignore_ascii_case(value, label) {
            let rest = rest.trim_start();
            return rest.strip_prefix(':').unwrap_or(rest).trim_start();
        }
    }
    value
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| if c == 'x' { 'X' } else { c })
        .collect()
}

fn isbn13_check_digit(first12: &[u8]) -> u8 {
    let sum: u32 = first12
        .iter()
        .enumerate()
        .map(|(index, digit)| u32::from(digit - b'0') * if index % 2 == 0 { 1 } else { 3 })
        .sum();
    ((10 - sum % 10) % 10) as u8
}

pub fn normalize_isbn(raw: &str) -> Check {
    let value = compact(strip_label(
        raw.trim(),
        &["ISBN-13", "ISBN-10", "ISBN13", "ISBN10", "ISBN"],
    ));
    let bytes = value.as_bytes();
    match bytes.len() {
        10 => {
            if !bytes[..9].iter().all(u8::is_ascii_digit)
                || !(bytes[9].is_ascii_digit() || bytes[9] == b'X')
            {
                return Err(invalid("format"));
            }
            let sum: u32 = bytes
                .iter()
                .enumerate()
                .map(|(index, b)| {
                    let digit = if *b == b'X' { 10 } else { u32::from(b - b'0') };
                    digit * (10 - index as u32)
                })
                .sum();
            if sum % 11 != 0 {
                return Err(invalid("checksum"));
            }
            let mut isbn13 = b"978".to_vec();
            isbn13.extend_from_slice(&bytes[..9]);
            let check = isbn13_check_digit(&isbn13);
            isbn13.push(b'0' + check);
            Ok(String::from_utf8(isbn13).expect("ascii digits"))
        }
        13 => {
            if !bytes.iter().all(u8::is_ascii_digit) {
                return Err(invalid("format"));
            }
            if !(bytes.starts_with(b"978") || bytes.starts_with(b"979")) {
                return Err(invalid("prefix"));
            }
            if isbn13_check_digit(&bytes[..12]) != bytes[12] - b'0' {
                return Err(invalid("checksum"));
            }
            Ok(value)
        }
        _ => Err(invalid("length")),
    }
}

pub fn normalize_issn(raw: &str) -> Check {
    let value = compact(strip_label(raw.trim(), &["ISSN"]));
    let bytes = value.as_bytes();
    if bytes.len() != 8
        || !bytes[..7].iter().all(u8::is_ascii_digit)
        || !(bytes[7].is_ascii_digit() || bytes[7] == b'X')
    {
        return Err(invalid("format"));
    }
    let sum: u32 = bytes
        .iter()
        .enumerate()
        .map(|(index, b)| {
            let digit = if *b == b'X' { 10 } else { u32::from(b - b'0') };
            digit * (8 - index as u32)
        })
        .sum();
    if sum % 11 != 0 {
        return Err(invalid("checksum"));
    }
    Ok(format!("{}-{}", &value[..4], &value[4..]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
    Year,
    Month,
    Day,
    Unparsed,
}

/// Publication date with explicit precision; never padded to a day or time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PartialDate {
    pub value: Option<String>,
    pub precision: DatePrecision,
    #[serde(default)]
    pub raw: Option<String>,
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

fn digits(value: &str, len: usize) -> Option<u32> {
    (value.len() == len && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// Validates the exact `YYYY`, `YYYY-MM` or `YYYY-MM-DD` form for `precision`.
fn check_value(value: &str, precision: DatePrecision) -> Result<(), &'static str> {
    let parts: Vec<&str> = value.split('-').collect();
    let expected = match precision {
        DatePrecision::Year => 1,
        DatePrecision::Month => 2,
        DatePrecision::Day => 3,
        DatePrecision::Unparsed => return Err("unparsed_has_value"),
    };
    if parts.len() != expected {
        return Err("precision_mismatch");
    }
    let year = digits(parts[0], 4).filter(|year| *year > 0).ok_or("year")?;
    if let Some(month) = parts.get(1) {
        let month = digits(month, 2)
            .filter(|m| (1..=12).contains(m))
            .ok_or("month")?;
        if let Some(day) = parts.get(2) {
            digits(day, 2)
                .filter(|d| *d >= 1 && *d <= days_in_month(year, month))
                .ok_or("day")?;
        }
    }
    Ok(())
}

impl PartialDate {
    pub fn validate(&self) -> Result<(), &'static str> {
        match (&self.value, self.precision) {
            (None, DatePrecision::Unparsed) => {
                if self.raw.as_deref().is_none_or(|raw| raw.trim().is_empty()) {
                    return Err("unparsed_without_raw");
                }
                Ok(())
            }
            (None, _) => Err("missing_value"),
            (Some(value), precision) => check_value(value, precision),
        }
    }

    /// Parses an imported/legacy string only when its form is unambiguous
    /// (year-first numeric or `YYYY年M月D日`). Anything else, including invalid
    /// Gregorian dates, stays unparsed with the raw text preserved.
    pub fn parse_legacy(raw: &str) -> Self {
        let trimmed = raw.trim();
        let parsed = parse_unambiguous(trimmed).and_then(|(value, precision)| {
            check_value(&value, precision)
                .ok()
                .map(|()| (value, precision))
        });
        match parsed {
            Some((value, precision)) => Self {
                value: Some(value),
                precision,
                raw: Some(raw.to_owned()),
            },
            None => Self {
                value: None,
                precision: DatePrecision::Unparsed,
                raw: Some(raw.to_owned()),
            },
        }
    }

    /// Legacy string projection: typed value, else raw text.
    pub fn legacy_string(&self) -> Option<&str> {
        self.value.as_deref().or(self.raw.as_deref())
    }

    /// Only use after verifying the item's native Zotero source identity.
    /// The prefix is a sorting aid, not sufficient evidence of an exact date.
    pub fn parse_zotero_multipart(raw: &str) -> Self {
        let ordinary = Self::parse_legacy(raw);
        if ordinary.precision != DatePrecision::Unparsed {
            return ordinary;
        }
        let Some((prefix, suffix)) = raw.split_once(' ') else { return ordinary };
        let bytes = prefix.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-'
            || !bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
            || suffix.is_empty() || suffix.trim() != suffix {
            return ordinary;
        }
        let normalized = if &prefix[5..] == "00-00" { &prefix[..4] }
            else if &prefix[8..] == "00" { &prefix[..7] } else { prefix };
        let candidate = Self::parse_legacy(normalized);
        if candidate.precision == DatePrecision::Unparsed { return ordinary; }
        // Multipart suffixes are dates, not SQL/RFC3339 timestamps or substrings.
        let suffix_date = if suffix.contains(':') {
            None
        } else {
            let parsed = Self::parse_legacy(suffix);
            if parsed.precision != DatePrecision::Unparsed { Some(parsed) }
            else { english_month_date(suffix) }
        };
        match suffix_date {
            Some(date) if date.value == candidate.value && date.precision == candidate.precision =>
                Self { raw: Some(raw.to_owned()), ..candidate },
            _ => ordinary,
        }
    }
}

fn english_month_date(raw: &str) -> Option<PartialDate> {
    let month = |word: &str| -> Option<usize> {
        let names = ["january", "february", "march", "april", "may", "june", "july",
            "august", "september", "october", "november", "december"];
        let lower = word.to_ascii_lowercase();
        names.iter().position(|name| lower == *name || lower == &name[..3]).map(|i| i + 1)
    };
    let fields: Vec<_> = raw.split_ascii_whitespace().collect();
    let value = match fields.as_slice() {
        [m, year] if digits(year, 4).is_some() => format!("{year}-{:02}", month(m)?),
        [m, day, year] if digits(year, 4).is_some() && month(m).is_some() => {
            let day = day.strip_suffix(',').unwrap_or(day);
            format!("{year}-{:02}-{}", month(m)?, pad2(day)?)
        }
        [day, m, year] if digits(year, 4).is_some() =>
            format!("{year}-{:02}-{}", month(m)?, pad2(day)?),
        _ => return None,
    };
    let parsed = PartialDate::parse_legacy(&value);
    (parsed.precision != DatePrecision::Unparsed).then_some(parsed)
}

impl crate::ItemMetadata {
    /// Actual publication day only; partial dates never imply a day or capture fallback.
    pub fn published_day(&self) -> Option<String> {
        self.effective_publication_date(false)
            .filter(|date| date.precision == DatePrecision::Day && date.validate().is_ok())
            .and_then(|date| date.value)
    }

    /// Shared read-only projection. Typed unparsed explicitly suppresses fallback.
    /// The boolean is supplied by a verified Store origin, never a client label.
    pub fn effective_publication_date(&self, native_zotero: bool) -> Option<PartialDate> {
        self.published_partial_date.as_ref().map(|date| date.value.clone()).or_else(|| {
            self.published_date.as_ref().map(|date| if native_zotero {
                PartialDate::parse_zotero_multipart(&date.value)
            } else { PartialDate::parse_legacy(&date.value) })
        })
    }
}

fn pad2(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 2 && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{value:0>2}"))
}

fn parse_unambiguous(value: &str) -> Option<(String, DatePrecision)> {
    if let Some((year, rest)) = value.split_once('年') {
        digits(year, 4)?;
        if rest.is_empty() {
            return Some((year.to_owned(), DatePrecision::Year));
        }
        let (month, rest) = rest.split_once('月')?;
        let month = pad2(month)?;
        if rest.is_empty() {
            return Some((format!("{year}-{month}"), DatePrecision::Month));
        }
        let day = pad2(rest.strip_suffix('日')?)?;
        return Some((format!("{year}-{month}-{day}"), DatePrecision::Day));
    }
    // RFC 3339 timestamp: keep only the literal calendar date it states.
    let date = match value.split_once('T') {
        Some((date, time)) if date.len() == 10 && valid_rfc3339_time(time) => date,
        Some(_) => return None,
        None => value,
    };
    let separator = if date.contains('/') { '/' } else { '-' };
    let parts: Vec<&str> = date.split(separator).collect();
    digits(parts[0], 4)?;
    match parts.as_slice() {
        [year] => Some(((*year).to_owned(), DatePrecision::Year)),
        [year, month] => Some((format!("{year}-{}", fixed2(month)?), DatePrecision::Month)),
        [year, month, day] => Some((
            format!("{year}-{}-{}", fixed2(month)?, fixed2(day)?),
            DatePrecision::Day,
        )),
        _ => None,
    }
}

fn fixed2(value: &str) -> Option<&str> {
    digits(value, 2).map(|_| value)
}

fn valid_rfc3339_time(time: &str) -> bool {
    let (clock, zone_ok) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        (clock, true)
    } else if let Some(index) = time.rfind(['+', '-']) {
        let zone = &time[index + 1..];
        let ok = zone.len() == 5
            && zone.as_bytes()[2] == b':'
            && digits(&zone[..2], 2).is_some_and(|h| h < 24)
            && digits(&zone[3..], 2).is_some_and(|m| m < 60);
        (&time[..index], ok)
    } else {
        return false;
    };
    let clock = clock.split_once('.').map_or(clock, |(main, fraction)| {
        if !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()) {
            main
        } else {
            ""
        }
    });
    let parts: Vec<&str> = clock.split(':').collect();
    zone_ok
        && parts.len() == 3
        && digits(parts[0], 2).is_some_and(|h| h < 24)
        && digits(parts[1], 2).is_some_and(|m| m < 60)
        && digits(parts[2], 2).is_some_and(|s| s <= 60)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstitutionIdentifier {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Institution {
    pub institution_id: String,
    pub name: String,
    #[serde(default)]
    pub identifiers: Vec<InstitutionIdentifier>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CitedReference {
    pub reference_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creators: Option<Vec<Creator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_date: Option<PartialDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifiers: Option<Vec<ItemIdentifier>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(check: Check) -> String {
        check.unwrap()
    }
    fn err(check: Check) -> (ValidationStatus, &'static str) {
        check.unwrap_err()
    }

    #[test]
    fn doi_wrappers_case_and_suffix_punctuation() {
        assert_eq!(
            ok(normalize_doi(" https://doi.org/10.1000/ABC(1)x. ")),
            "10.1000/abc(1)x."
        );
        assert_eq!(ok(normalize_doi("DOI: 10.1000/A/B/C")), "10.1000/a/b/c");
        assert_eq!(
            ok(normalize_doi("http://DX.DOI.ORG/10.1000.5/x")),
            "10.1000.5/x"
        );
        // Decoded exactly once: %2523 → %23, not '#'.
        assert_eq!(
            ok(normalize_doi("https://doi.org/10.1000/a%2523b")),
            "10.1000/a%23b"
        );
        assert_eq!(
            ok(normalize_doi("https://doi.org/10.1000/a%3Fb")),
            "10.1000/a?b"
        );
        // Terminal punctuation and long suffixes survive untruncated.
        let long = format!("10.1000/{}", "y".repeat(500));
        assert_eq!(ok(normalize_doi(&format!("{long};"))), format!("{long};"));
        assert_eq!(
            err(normalize_doi("https://user@doi.org/10.1/x")).1,
            "url_credentials"
        );
        assert_eq!(
            err(normalize_doi("https://doi.org/10.1/x?y=1")).1,
            "url_query_or_fragment"
        );
        assert_eq!(
            err(normalize_doi("https://doi.org/10.1/x#frag")).1,
            "url_query_or_fragment"
        );
        assert_eq!(
            err(normalize_doi("https://example.org/10.1/x")).1,
            "not_doi_resolver"
        );
        assert_eq!(
            err(normalize_doi("https://doi.org.evil/10.1/x")).1,
            "not_doi_resolver"
        );
        assert_eq!(
            err(normalize_doi("https://doi.org/10.1/%zz")).1,
            "bad_percent_encoding"
        );
        assert_eq!(err(normalize_doi("10.1000/a b")).1, "whitespace_or_control");
        assert_eq!(
            err(normalize_doi("10.1000/a\u{7}")).1,
            "whitespace_or_control"
        );
        assert_eq!(err(normalize_doi("11.1000/x")).1, "prefix");
        assert_eq!(err(normalize_doi("10.ab/x")).1, "prefix");
        assert_eq!(err(normalize_doi("10..1/x")).1, "prefix");
        assert_eq!(err(normalize_doi("10.1000/")).1, "missing_suffix");
        assert_eq!(err(normalize_doi("10.1000")).1, "missing_suffix");
        assert_eq!(
            err(normalize_doi("10.1000/Ärger")),
            (ValidationStatus::Unparsed, "non_ascii_case")
        );
        // Caseless non-ASCII is fine and preserved.
        assert_eq!(ok(normalize_doi("10.1000/中文X")), "10.1000/中文x");
    }

    #[test]
    fn isbn_checksums_978_equivalence_and_979_boundary() {
        assert_eq!(ok(normalize_isbn("0-306-40615-2")), "9780306406157");
        assert_eq!(
            ok(normalize_isbn("ISBN-13: 978-0-306-40615-7")),
            "9780306406157"
        );
        assert_eq!(ok(normalize_isbn("isbn 0 306 40615 2")), "9780306406157");
        // Lowercase x check digit uppercases; 080442957X ↔ 9780804429573.
        assert_eq!(ok(normalize_isbn("0-8044-2957-x")), "9780804429573");
        assert_eq!(err(normalize_isbn("0-306-40615-3")).1, "checksum");
        assert_eq!(err(normalize_isbn("978-0-306-40615-8")).1, "checksum");
        assert_eq!(err(normalize_isbn("X306406152")).1, "format");
        // 979 is valid as ISBN-13 and has no ISBN-10 form.
        assert_eq!(ok(normalize_isbn("979-10-90636-07-1")), "9791090636071");
        assert_eq!(err(normalize_isbn("9771234567898")).1, "prefix");
        assert_eq!(err(normalize_isbn("12345")).1, "length");
        let isbn = normalize_identifier(IdentifierKind::Isbn, "0-306-40615-2");
        assert_eq!(isbn.comparison_key.as_deref(), Some("9780306406157"));
        assert_eq!(isbn.validation.status, ValidationStatus::Valid);
        assert_eq!(isbn.validation.reason, None);
    }

    #[test]
    fn issn_checksum_display_and_no_comparison_key() {
        assert_eq!(ok(normalize_issn("0317-8471")), "0317-8471");
        assert_eq!(ok(normalize_issn("2434561x")), "2434-561X");
        assert_eq!(err(normalize_issn("0317-8472")).1, "checksum");
        let issn = normalize_identifier(IdentifierKind::Issn, "0317 8471");
        assert_eq!(issn.value.as_deref(), Some("0317-8471"));
        assert_eq!(issn.comparison_key, None);
        let bad = normalize_identifier(IdentifierKind::Doi, "10.1000");
        assert_eq!(bad.validation.status, ValidationStatus::Invalid);
        assert_eq!((bad.value, bad.comparison_key), (None, None));
        let pmid = normalize_identifier(IdentifierKind::Pmid, " 123 ");
        assert_eq!(pmid.validation.status, ValidationStatus::Unparsed);
        assert_eq!(pmid.comparison_key, None);
    }

    #[test]
    fn legacy_revision_roundtrips_byte_identically_without_fabrication() {
        // Shape written before bibliographic v2 (string date, display-only creators).
        let legacy = r#"{
  "schema_version": 1,
  "revision_id": "revision_legacy",
  "item_id": "item_legacy",
  "revision_number": 1,
  "title": "Legacy",
  "source_url": "https://example.invalid/a",
  "canonical_url": null,
  "content_markdown": "body",
  "sensitivity": "public",
  "evidence_ids": [],
  "provenance": [],
  "metadata": {
    "creators": {
      "value": [
        {
          "display_name": "李 Ada",
          "role": "author"
        }
      ],
      "provenance": {
        "origin": "json_ld",
        "provider_id": "p",
        "provider_version": "1"
      }
    },
    "published_date": {
      "value": "Spring 2024",
      "provenance": {
        "origin": "citation_meta",
        "provider_id": "p",
        "provider_version": "1"
      }
    },
    "identifiers": {
      "value": [
        {
          "kind": "doi",
          "value": "10.1000/ABC"
        }
      ],
      "provenance": {
        "origin": "citation_meta",
        "provider_id": "p",
        "provider_version": "1"
      }
    }
  }
}"#;
        let revision: crate::ItemRevision = serde_json::from_str(legacy).unwrap();
        assert_eq!(serde_json::to_string_pretty(&revision).unwrap(), legacy);
        // revision_sha256 is computed over the compact form.
        let compact = legacy
            .lines()
            .map(str::trim_start)
            .collect::<String>()
            .replace("\": ", "\":");
        assert_eq!(serde_json::to_string(&revision).unwrap(), compact);
        let metadata = &revision.metadata;
        let creator = &metadata.creators.as_ref().unwrap().value[0];
        assert_eq!(creator, &Creator::new("李 Ada", crate::CreatorRole::Author));
        assert_eq!(metadata.published_partial_date, None);
        assert_eq!(
            metadata.published_date.as_ref().unwrap().value,
            "Spring 2024"
        );
        let identifier = &metadata.identifiers.as_ref().unwrap().value[0];
        assert_eq!(
            (identifier.raw_value.as_ref(), identifier.qualifier.as_ref()),
            (None, None)
        );
        assert_eq!(metadata.journal_title, None);
        assert_eq!(metadata.cited_references, None);
    }

    fn date(value: Option<&str>, precision: DatePrecision) -> PartialDate {
        PartialDate {
            value: value.map(str::to_owned),
            precision,
            raw: None,
        }
    }

    #[test]
    fn partial_dates_validate_precision_and_leap_days() {
        assert!(date(Some("2024-02-29"), DatePrecision::Day)
            .validate()
            .is_ok());
        assert!(date(Some("2000-02-29"), DatePrecision::Day)
            .validate()
            .is_ok());
        assert_eq!(
            date(Some("2023-02-29"), DatePrecision::Day).validate(),
            Err("day")
        );
        assert_eq!(
            date(Some("1900-02-29"), DatePrecision::Day).validate(),
            Err("day")
        );
        assert_eq!(
            date(Some("2024-04-31"), DatePrecision::Day).validate(),
            Err("day")
        );
        assert_eq!(
            date(Some("2024-13"), DatePrecision::Month).validate(),
            Err("month")
        );
        assert_eq!(
            date(Some("2024-01-01"), DatePrecision::Year).validate(),
            Err("precision_mismatch")
        );
        assert_eq!(
            date(Some("2024"), DatePrecision::Day).validate(),
            Err("precision_mismatch")
        );
        assert!(date(Some("2024"), DatePrecision::Year).validate().is_ok());
        assert_eq!(
            date(Some("2024-2"), DatePrecision::Month).validate(),
            Err("month")
        );
        assert_eq!(
            date(None, DatePrecision::Year).validate(),
            Err("missing_value")
        );
        assert_eq!(
            date(None, DatePrecision::Unparsed).validate(),
            Err("unparsed_without_raw")
        );
        assert_eq!(
            date(Some("2024"), DatePrecision::Unparsed).validate(),
            Err("unparsed_has_value")
        );
    }

    #[test]
    fn multipart_requires_matching_suffix_and_precision() {
        for (raw, value, precision) in [
            ("2024-00-00 2024", "2024", DatePrecision::Year),
            ("2024-02-00 2024年2月", "2024-02", DatePrecision::Month),
            ("2024-10-00 OCT 2024", "2024-10", DatePrecision::Month),
            ("2024-02-29 February 29, 2024", "2024-02-29", DatePrecision::Day),
            ("2024-02-29 29 Feb 2024", "2024-02-29", DatePrecision::Day),
            ("2024-02-29 2024/02/29", "2024-02-29", DatePrecision::Day),
            ("2024-02-29 2024年2月29日", "2024-02-29", DatePrecision::Day),
        ] {
            let parsed = PartialDate::parse_zotero_multipart(raw);
            assert_eq!((parsed.value.as_deref(), parsed.precision), (Some(value), precision), "{raw}");
            assert_eq!(parsed.raw.as_deref(), Some(raw));
            assert_eq!(PartialDate::parse_legacy(raw).precision, DatePrecision::Unparsed);
        }
        for raw in [
            "0000-00-00 0000", "2024-00-02 2024", "2024-13-00 2024",
            "1900-02-29 February 29, 1900", "2023-02-29 2023年2月29日",
            "2024-02-29 2024", "2024-00-00 February 2024", "2024-02-00 March 2024",
            "2024-00-00 circa 2024", "2024-00-00 2024–2025", "2024-00-00 Spring 2024",
            "2024-02-29 02/29/2024", "2024-02-29 12:00:00", "2024-00-00 2024?",
            "2024-02-29 2024-02-29T12:00:00Z", "2024-02-29 February 29, 2024 updated",
        ] {
            let parsed = PartialDate::parse_zotero_multipart(raw);
            assert_eq!(parsed.precision, DatePrecision::Unparsed, "{raw}");
            assert_eq!(parsed.raw.as_deref(), Some(raw));
        }
        assert_eq!(PartialDate::parse_zotero_multipart("2024年2月"), PartialDate::parse_legacy("2024年2月"));
    }

    #[test]
    fn effective_publication_preserves_typed_unparsed_and_requires_verified_origin() {
        use crate::{ItemMetadata, MetadataOrigin, MetadataProvenance, Sourced};
        let provenance = MetadataProvenance::new(MetadataOrigin::IntakeProvided, "synthetic", "1");
        let mut metadata = ItemMetadata { published_date: Some(Sourced {
            value: "2024-00-00 2024".into(), provenance: provenance.clone(),
        }), ..Default::default() };
        assert_eq!(metadata.effective_publication_date(false).unwrap().precision, DatePrecision::Unparsed);
        assert_eq!(metadata.effective_publication_date(true).unwrap().precision, DatePrecision::Year);
        let unknown = PartialDate::parse_legacy("circa 2024");
        metadata.published_partial_date = Some(Sourced { value: unknown.clone(), provenance });
        assert_eq!(metadata.effective_publication_date(true), Some(unknown));
    }

    #[test]
    fn legacy_dates_parse_only_when_unambiguous() {
        let year = PartialDate::parse_legacy("2024");
        assert_eq!(
            (year.value.as_deref(), year.precision),
            (Some("2024"), DatePrecision::Year)
        );
        let month = PartialDate::parse_legacy("2024年2月");
        assert_eq!(month.value.as_deref(), Some("2024-02"));
        assert_eq!(month.raw.as_deref(), Some("2024年2月"));
        let stamp = PartialDate::parse_legacy("2024-02-29T23:30:00-05:00");
        assert_eq!(
            (stamp.value.as_deref(), stamp.precision),
            (Some("2024-02-29"), DatePrecision::Day)
        );
        for ambiguous in [
            "03/04/2024",
            "Spring 2024",
            "2023-02-29",
            "c. 1900",
            "2024-02-29T25:00:00Z",
        ] {
            let parsed = PartialDate::parse_legacy(ambiguous);
            assert_eq!(parsed.precision, DatePrecision::Unparsed, "{ambiguous}");
            assert_eq!(parsed.value, None);
            assert_eq!(parsed.legacy_string(), Some(ambiguous));
        }
    }
}
