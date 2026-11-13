//! Reading passes and optional device-reported active dwell time (`reader_passes_v1`).
//!
//! Metadata lives in `catalog/reader-passes/<item_id>.json` (per-item CAS, ordinals,
//! tombstones, last-mutation receipt). Accepted timing intervals live per pass in
//! `catalog/reader-passes/<item_id>.timing/<pass_id>.json` with their own version, so a
//! heartbeat never invalidates an unrelated manual-date CAS. Both are written only
//! under the Store writer lock. Live timing sessions are process memory bound to the
//! daemon boot; they are never persisted, backed up or restored.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use unicode_segmentation::UnicodeSegmentation;

const SCHEMA: u32 = 1;
pub const MAX_PASSES: usize = 4096;
/// Retained pass identities (live + tombstones). Deletion never grows it.
pub const MAX_PASS_IDENTITIES: usize = 65536;
/// Creating a pass is refused once the serialized metadata would exceed this. Edit,
/// complete, reopen and delete stay available above it: records are individually
/// bounded, so the file cannot exceed `MAX_LEDGER_FILE_BYTES` (worst case ≈ 4096 ×
/// 1.3 KiB passes + 61440 × 170 B tombstones ≈ 16 MiB).
pub const MAX_LEDGER_BYTES: usize = 12 * 1024 * 1024;
const MAX_LEDGER_FILE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_SEGMENTS: usize = 65536;
pub const MAX_TIMING_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_REPORT_SEGMENTS: usize = 64;
pub const MAX_REPORT_BYTES: usize = 32 * 1024;
const MAX_PAGE_BYTES: usize = 48 * 1024;
pub const SESSION_WINDOW_MS: i64 = 300_000;
pub const MAX_SEGMENT_MS: i64 = 300_000;
/// Reports for a session are admitted until window + grace of server monotonic time.
pub const REPORT_GRACE_MS: i64 = 60_000;
/// Allowed divergence between server UTC and monotonic elapsed time in one session.
pub const CLOCK_TOLERANCE_MS: i64 = 2_000;
const MAX_SESSIONS: usize = 4096;
pub const MAX_ESTIMATE_MINUTES: u32 = 525_600;
pub const COUNT_POLICY: &str = "memex-pass-count-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind { Tracked, Retrospective }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method { Full, Sampled, Structural }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status { InProgress, Completed }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision { Year, Month, Day }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayUnit { Minutes, Hours }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Source, Bilingual, Translation }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionBasis { StartedOnRevision, ReferenceAtEntry }

/// Owner-entered manual date in the metadata PartialDate shape (no unparsed writes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassDate {
    pub value: String,
    pub precision: Precision,
    #[serde(default)]
    pub raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditionLength {
    pub side: String,
    pub sha256: String,
    pub words: u64,
    pub characters: u64,
}

/// Frozen at pass creation from the exactly bound edition hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lengths {
    pub policy: String,
    pub source: Option<EditionLength>,
    pub translation: Option<EditionLength>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pass {
    pass_id: String,
    ordinal: u64,
    kind: Kind,
    status: Status,
    method: Option<Method>,
    start_date: Option<PassDate>,
    end_date: Option<PassDate>,
    estimated_duration_minutes: Option<u32>,
    duration_display_unit: Option<DisplayUnit>,
    timezone: String,
    revision_basis: Option<RevisionBasis>,
    revision_id: Option<String>,
    canonical_sha256: Option<String>,
    lengths: Option<Lengths>,
    created_at_ms: i64,
    completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {
    pass_id: String,
    ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    CreateTracked {
        pass_id: String,
        revision_id: String,
        method: Option<Method>,
        start_date: Option<PassDate>,
        timezone: String,
    },
    CreateRetrospective {
        pass_id: String,
        method: Option<Method>,
        start_date: Option<PassDate>,
        end_date: Option<PassDate>,
        estimated_duration_minutes: Option<u32>,
        duration_display_unit: Option<DisplayUnit>,
        timezone: String,
        reference_revision_id: Option<String>,
    },
    Update {
        pass_id: String,
        method: Option<Method>,
        start_date: Option<PassDate>,
        end_date: Option<PassDate>,
        estimated_duration_minutes: Option<u32>,
        duration_display_unit: Option<DisplayUnit>,
    },
    Complete { pass_id: String, end_date: Option<PassDate>, expected_timing_version: Option<String> },
    Reopen { pass_id: String },
    Delete { pass_id: String, expected_timing_version: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditResult {
    pub item_id: String,
    pub version: String,
    pub pass_id: String,
    pub ordinal: u64,
    /// `created`, `updated`, `completed`, `reopened` or `deleted`.
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    principal: String,
    expected_version: String,
    change_sha256: String,
    result: EditResult,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    item_id: String,
    sequence: u64,
    /// Last allocated ordinal; never decreases or reuses a deleted ordinal.
    last_ordinal: u64,
    passes: Vec<Pass>,
    tombstones: Vec<Tombstone>,
    last: Option<Receipt>,
}

impl Ledger {
    /// Sequence alone repeats after a backup restore; binding the content too keeps
    /// a pre-restore version from passing CAS on different records.
    fn version(&self) -> String {
        let body = serde_json::to_string(&(&self.passes, &self.tombstones)).expect("passes serialise");
        reader_sha256(
            format!("reader-passes-v1\0{}\0{}\0{}\0{}", self.item_id, self.sequence, self.last_ordinal, body).as_bytes(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    session_id: String,
    principal: String,
    segment_id: String,
    payload_sha256: String,
    mode: Mode,
    /// Server UTC milliseconds, half-open.
    start_ms: i64,
    end_ms: i64,
    /// Server acceptance order; the earliest accepted interval owns overlapping time.
    seq: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Timing {
    schema_version: u32,
    item_id: String,
    pass_id: String,
    sequence: u64,
    segments: Vec<Accepted>,
}

impl Timing {
    fn version(&self) -> String {
        let body = serde_json::to_string(&self.segments).expect("segments serialise");
        reader_sha256(
            format!("reader-pass-timing-v1\0{}\0{}\0{}\0{}", self.item_id, self.pass_id, self.sequence, body).as_bytes(),
        )
    }
}

/// Server clock sample supplied by the daemon. `mono_ms` must be monotonic within one boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimingClock {
    pub boot_id: String,
    pub utc_ms: i64,
    pub mono_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentReport {
    pub segment_id: String,
    pub mode: Mode,
    /// Client monotonic offsets from the moment it received the session grant.
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingSession {
    pub session_id: String,
    pub item_id: String,
    pub pass_id: String,
    pub revision_id: String,
    pub origin_utc_ms: i64,
    pub window_ms: i64,
    pub max_segment_ms: i64,
    pub report_deadline_utc_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingTotals {
    pub timing_version: String,
    pub accepted_ms: i64,
    pub unique_ms: i64,
    pub overlap_excluded_ms: i64,
    pub source_ms: i64,
    pub bilingual_ms: i64,
    pub translation_ms: i64,
    pub segment_count: usize,
    pub segment_limit: usize,
    /// At the per-pass identity limit: new recording is paused, prior data kept.
    pub recording_paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentOutcome {
    pub segment_id: String,
    /// `accepted` or `duplicate` (exact replay, no-op).
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportResult {
    pub item_id: String,
    pub pass_id: String,
    pub session_id: String,
    pub outcomes: Vec<SegmentOutcome>,
    pub totals: TimingTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitRates {
    pub words_per_minute: Option<f64>,
    pub characters_per_minute: Option<f64>,
}

/// Equivalent full-text rates: length / T with ONE denominator, never T / length.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// `measured` (tracked, unique active time) or `retrospective_estimate`.
    pub basis: String,
    pub minutes: Option<f64>,
    /// Always true unless a completed `full` tracked pass; even then self-reported.
    pub estimate: bool,
    pub equivalent_full_text: bool,
    pub source: UnitRates,
    pub translation: UnitRates,
    /// Per-unit sums of source + translation only when both lengths exist.
    pub combined: UnitRates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListedPass {
    pub pass_id: String,
    pub ordinal: u64,
    pub display_rank: usize,
    pub kind: Kind,
    pub status: Status,
    pub method: Option<Method>,
    pub start_date: Option<PassDate>,
    pub end_date: Option<PassDate>,
    pub estimated_duration_minutes: Option<u32>,
    pub duration_display_unit: Option<DisplayUnit>,
    pub timezone: String,
    pub revision_basis: Option<RevisionBasis>,
    pub revision_id: Option<String>,
    pub canonical_sha256: Option<String>,
    /// `current`, `changed`, or null when no reference was frozen.
    pub source_status: Option<String>,
    pub lengths: Option<Lengths>,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub timing: TimingTotals,
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassPage {
    pub item_id: String,
    pub version: String,
    pub current_revision_id: String,
    pub passes: Vec<ListedPass>,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub pass_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayTotal {
    pub date: String,
    pub ms: i64,
    pub source_ms: i64,
    pub bilingual_ms: i64,
    pub translation_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayPage {
    pub item_id: String,
    pub pass_id: String,
    pub timezone: String,
    pub timing_version: String,
    pub days: Vec<DayTotal>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

fn ledger_path(root: &Path, item: &str) -> PathBuf {
    root.join("catalog/reader-passes").join(format!("{item}.json"))
}
fn timing_dir(item: &str) -> PathBuf {
    Path::new("catalog/reader-passes").join(format!("{item}.timing"))
}
fn timing_path(root: &Path, item: &str, pass: &str) -> PathBuf {
    root.join(timing_dir(item)).join(format!("{pass}.json"))
}

fn unavailable() -> anyhow::Error {
    anyhow::anyhow!("reader_passes_unavailable")
}

/// Missing file is the empty ledger; anything unreadable or unsupported fails closed.
fn ledger(root: &Path, item: &str) -> Result<Ledger> {
    let p = ledger_path(root, item);
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ledger { schema_version: SCHEMA, item_id: item.into(), ..Default::default() })
        }
        Err(e) => return Err(e.into()),
        Ok(m) if m.len() as usize > MAX_LEDGER_FILE_BYTES => return Err(unavailable()),
        Ok(_) => {}
    }
    let l: Ledger = load(root, p).map_err(|_| unavailable())?;
    let mut ids = BTreeSet::new();
    let ok = l.schema_version == SCHEMA
        && l.item_id == item
        && l.passes.len() <= MAX_PASSES
        && l.passes.iter().map(|p| (&p.pass_id, p.ordinal)).chain(l.tombstones.iter().map(|t| (&t.pass_id, t.ordinal)))
            .all(|(id, ordinal)| valid_pass_id(id) && ordinal >= 1 && ordinal <= l.last_ordinal && ids.insert(id.clone()));
    anyhow::ensure!(ok, "reader_passes_unavailable");
    Ok(l)
}

fn timing(root: &Path, item: &str, pass: &str) -> Result<Timing> {
    let p = timing_path(root, item, pass);
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Timing { schema_version: SCHEMA, item_id: item.into(), pass_id: pass.into(), ..Default::default() })
        }
        Err(e) => return Err(e.into()),
        Ok(m) if m.len() as usize > MAX_TIMING_BYTES + 64 * 1024 => return Err(unavailable()),
        Ok(_) => {}
    }
    let t: Timing = load(root, p).map_err(|_| unavailable())?;
    let ok = t.schema_version == SCHEMA
        && t.item_id == item
        && t.pass_id == pass
        && t.segments.len() <= MAX_SEGMENTS
        && t.segments.iter().all(|s| s.start_ms < s.end_ms && s.end_ms - s.start_ms <= MAX_SEGMENT_MS && s.seq >= 1 && s.seq <= t.sequence);
    anyhow::ensure!(ok, "reader_passes_unavailable");
    Ok(t)
}

fn valid_pass_id(id: &str) -> bool {
    id.len() <= 128 && id.len() > 5 && safe_component(id, "pass_")
}
fn valid_segment_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && safe_component(id, "")
}
fn valid_timezone(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
        && jiff::tz::TimeZone::get(name).is_ok()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Earliest and latest possible calendar days of a validated PartialDate.
fn bounds(d: &PassDate) -> ((u32, u32, u32), (u32, u32, u32)) {
    let parts: Vec<u32> = d.value.split('-').map(|p| p.parse().unwrap_or(0)).collect();
    let year = parts[0];
    match d.precision {
        Precision::Year => ((year, 1, 1), (year, 12, 31)),
        Precision::Month => ((year, parts[1], 1), (year, parts[1], days_in_month(year, parts[1]))),
        Precision::Day => ((year, parts[1], parts[2]), (year, parts[1], parts[2])),
    }
}

fn valid_date(d: &PassDate) -> bool {
    let precision = match d.precision {
        Precision::Year => memex_core::DatePrecision::Year,
        Precision::Month => memex_core::DatePrecision::Month,
        Precision::Day => memex_core::DatePrecision::Day,
    };
    PartialDate { value: Some(d.value.clone()), precision, raw: d.raw.clone() }.validate().is_ok()
        && d.raw.as_ref().is_none_or(|raw| raw.len() <= 256 && !raw.chars().any(char::is_control))
}

struct Editable<'a> {
    kind: Kind,
    status: Status,
    start: &'a Option<PassDate>,
    end: &'a Option<PassDate>,
    estimate: Option<u32>,
    unit: Option<DisplayUnit>,
}

/// Field rules shared by create/update/complete. Never rounds, clips or invents values.
fn validate_fields(f: &Editable<'_>) -> Result<()> {
    let dates_ok = f.start.iter().chain(f.end.iter()).all(valid_date);
    let estimate_ok = match f.kind {
        Kind::Tracked => f.estimate.is_none() && f.unit.is_none(),
        Kind::Retrospective => {
            f.estimate.is_none_or(|m| (1..=MAX_ESTIMATE_MINUTES).contains(&m)) && (f.unit.is_none() || f.estimate.is_some())
        }
    };
    anyhow::ensure!(
        dates_ok && estimate_ok && !(f.status == Status::InProgress && f.end.is_some()),
        "invalid_reader_pass"
    );
    if let (Some(start), Some(end)) = (f.start, f.end) {
        // Reject only an impossible order; overlapping precision ranges stay uncertain.
        anyhow::ensure!(bounds(end).1 >= bounds(start).0, "invalid_reader_pass_dates");
    }
    Ok(())
}

/// Item visible at `ceiling` and current reader projection, or not found.
fn current(root: &Path, item: &str, ceiling: Sensitivity) -> Result<(ReaderDocument, ReaderBodies)> {
    let (_, revision) = read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    document_revision_locked(root, &revision, ceiling)
}

/// A tracked pass started on the current revision, or on a revision whose body is the current
/// body (only metadata such as the title edited since), keeps timing.
fn same_body_pass(root: &Path, item: &str, p: &Pass, doc: &ReaderDocument) -> bool {
    p.revision_id.as_deref().is_some_and(|r| {
        p.canonical_sha256.as_deref() == Some(&doc.canonical_sha256)
            && crate::body::same_body(root, item, &doc.revision_id, r)
    })
}

/// A pass bound to an older revision is disclosed/edited only when that historical
/// source is readable at this ceiling as well; never weaker than history access.
fn authorized(root: &Path, item: &str, pass: &Pass, doc: &ReaderDocument, ceiling: Sensitivity, cache: &mut BTreeMap<String, bool>) -> bool {
    let Some(revision) = &pass.revision_id else { return true };
    if *revision == doc.revision_id {
        return true;
    }
    *cache.entry(revision.clone()).or_insert_with(|| {
        revision_text::historical_revision(root, item, revision, ceiling)
            .and_then(|old| document_revision_locked(root, &old, ceiling))
            .is_ok()
    })
}

/// CJK letters for `memex-pass-count-v1`: Han ideographs and iteration/closing marks,
/// Hiragana/Katakana letters with the prolonged sound mark and iteration marks, Bopomofo
/// and Hangul. Punctuation (・ ゠ 、 。 brackets), standalone voicing marks ゛゜ and
/// full-width symbols are not letters and never count.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        // Han, including 々 〆 〇 and 〻.
        0x3005..=0x3007 | 0x303B | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x20000..=0x2FA1F | 0x30000..=0x323AF
        // Hiragana letters, ゝ ゞ ゟ.
        | 0x3041..=0x3096 | 0x309D..=0x309F
        // Katakana letters, ー, ヽ ヾ ヿ; phonetic extensions; half-width letters and ｰ.
        | 0x30A1..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9D
        // Bopomofo.
        | 0x3105..=0x312F | 0x31A0..=0x31BF
        // Hangul jamo, compatibility jamo, syllables and half-width letters;
        // fillers (U+115F, U+1160, U+3164, U+FFA0) are excluded.
        | 0x1100..=0x115E | 0x1161..=0x11FF | 0x3131..=0x3163 | 0x3165..=0x318E | 0xA960..=0xA97C
        | 0xAC00..=0xD7A3 | 0xD7B0..=0xD7FB | 0xFFA1..=0xFFDC)
}

/// `memex-pass-count-v1`: visible Markdown text (headings, lists, tables, code; not
/// syntax, link targets, image alt/URLs or raw HTML). Words are UAX #29 words with no
/// CJK letter; characters are extended grapheme clusters starting with a CJK letter.
/// Units are reported separately and never added together.
pub fn count_text(markdown: &str) -> (u64, u64) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut text = String::new();
    let mut image = 0usize;
    for event in Parser::new_ext(markdown, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS) {
        match event {
            Event::Start(Tag::Image { .. }) => image += 1,
            Event::End(TagEnd::Image) => image = image.saturating_sub(1),
            Event::Text(t) | Event::Code(t) if image == 0 => text.push_str(&t),
            Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link) => {}
            Event::SoftBreak | Event::HardBreak | Event::Rule | Event::End(_) => text.push('\n'),
            _ => {}
        }
    }
    let words = text.unicode_words().filter(|w| !w.chars().any(is_cjk)).count() as u64;
    let characters = text.graphemes(true).filter(|g| g.chars().next().is_some_and(is_cjk)).count() as u64;
    (words, characters)
}

fn lengths(doc: &ReaderDocument, bodies: &ReaderBodies) -> Lengths {
    let of = |sides: &[&str]| {
        doc.editions.iter().find(|e| sides.contains(&e.side.as_str())).and_then(|e| {
            let (body, _) = bodies.get(&e.side)?;
            (reader_sha256(body.as_bytes()) == e.sha256).then(|| {
                let (words, characters) = count_text(body);
                EditionLength { side: e.side.clone(), sha256: e.sha256.clone(), words, characters }
            })
        })
    };
    Lengths { policy: COUNT_POLICY.into(), source: of(&["source", "original"]), translation: of(&["translation"]) }
}

/// Union of all accepted intervals; overlapping instants belong to the first accepted.
fn owned(segments: &[Accepted]) -> Vec<(i64, i64, Mode)> {
    let mut order: Vec<&Accepted> = segments.iter().collect();
    order.sort_by_key(|s| s.seq);
    let mut covered: BTreeMap<i64, i64> = BTreeMap::new();
    let mut out = Vec::new();
    for s in order {
        let (mut lo, mut hi) = (s.start_ms, s.end_ms);
        let overlapping: Vec<(i64, i64)> = covered
            .range(..s.end_ms)
            .rev()
            .take_while(|(_, end)| **end >= s.start_ms)
            .map(|(a, b)| (*a, *b))
            .collect();
        let mut cursor = s.start_ms;
        for (a, b) in overlapping.iter().rev() {
            if *a > cursor {
                out.push((cursor, *a, s.mode));
            }
            cursor = cursor.max(*b);
            lo = lo.min(*a);
            hi = hi.max(*b);
            covered.remove(a);
        }
        if cursor < s.end_ms {
            out.push((cursor, s.end_ms, s.mode));
        }
        covered.insert(lo, hi);
    }
    out
}

fn totals(t: &Timing) -> TimingTotals {
    let accepted_ms = t.segments.iter().map(|s| s.end_ms - s.start_ms).sum::<i64>();
    let mut modes = [0i64; 3];
    for (a, b, mode) in owned(&t.segments) {
        modes[mode as usize] += b - a;
    }
    let unique_ms = modes.iter().sum::<i64>();
    TimingTotals {
        timing_version: t.version(),
        accepted_ms,
        unique_ms,
        overlap_excluded_ms: accepted_ms - unique_ms,
        source_ms: modes[0],
        bilingual_ms: modes[1],
        translation_ms: modes[2],
        segment_count: t.segments.len(),
        segment_limit: MAX_SEGMENTS,
        recording_paused: t.segments.len() >= MAX_SEGMENTS,
    }
}

/// Splits owned time at the pinned zone's local midnights (23/25-hour DST days).
fn days(t: &Timing, zone: &str) -> Result<Vec<DayTotal>> {
    let tz = jiff::tz::TimeZone::get(zone).map_err(|_| unavailable())?;
    let mut by_day: BTreeMap<String, [i64; 3]> = BTreeMap::new();
    for (a, b, mode) in owned(&t.segments) {
        let mut cursor = a;
        while cursor < b {
            let date = jiff::Timestamp::from_millisecond(cursor).map_err(|_| unavailable())?.to_zoned(tz.clone()).date();
            let next = date
                .tomorrow()
                .and_then(|d| d.to_zoned(tz.clone()))
                .map_err(|_| unavailable())?
                .timestamp()
                .as_millisecond();
            let end = b.min(next);
            by_day.entry(date.to_string()).or_default()[mode as usize] += end - cursor;
            cursor = end;
        }
    }
    Ok(by_day
        .into_iter()
        .map(|(date, m)| DayTotal { date, ms: m.iter().sum(), source_ms: m[0], bilingual_ms: m[1], translation_ms: m[2] })
        .collect())
}

fn rates(p: &Pass, unique_ms: i64) -> Rates {
    let (basis, minutes) = match p.kind {
        Kind::Tracked => ("measured", (unique_ms > 0).then(|| unique_ms as f64 / 60_000.0)),
        Kind::Retrospective => ("retrospective_estimate", p.estimated_duration_minutes.map(f64::from)),
    };
    let per = |words: Option<u64>, characters: Option<u64>| UnitRates {
        words_per_minute: minutes.zip(words.filter(|w| *w > 0)).map(|(m, w)| w as f64 / m),
        characters_per_minute: minutes.zip(characters.filter(|c| *c > 0)).map(|(m, c)| c as f64 / m),
    };
    let source = p.lengths.as_ref().and_then(|l| l.source.as_ref());
    let translation = p.lengths.as_ref().and_then(|l| l.translation.as_ref());
    let both = source.zip(translation);
    Rates {
        basis: basis.into(),
        minutes,
        estimate: p.kind == Kind::Retrospective || p.method != Some(Method::Full) || p.status != Status::Completed,
        equivalent_full_text: p.method != Some(Method::Full),
        source: per(source.map(|l| l.words), source.map(|l| l.characters)),
        translation: per(translation.map(|l| l.words), translation.map(|l| l.characters)),
        combined: per(both.map(|(s, t)| s.words + t.words), both.map(|(s, t)| s.characters + t.characters)),
    }
}

/// Known starts first by normalized value (shorter prefix first), then ordinal and ID;
/// unknown starts last. Derived for display only; never an identity.
fn ranked<'a>(passes: impl Iterator<Item = &'a Pass>) -> Vec<&'a Pass> {
    let mut v: Vec<&Pass> = passes.collect();
    v.sort_by(|a, b| {
        let key = |p: &'a Pass| (p.start_date.is_none(), p.start_date.as_ref().map(|d| d.value.clone()), p.ordinal, p.pass_id.clone());
        key(a).cmp(&key(b))
    });
    v
}

pub fn list(
    library: &Path,
    item: &str,
    offset: usize,
    limit: usize,
    expected_version: Option<&str>,
    ceiling: Sensitivity,
) -> Result<PassPage> {
    anyhow::ensure!(
        valid_item_id(item) && (1..=100).contains(&limit) && (offset == 0 || expected_version.is_some()),
        "invalid_reader_pass"
    );
    with_consistent_snapshot(library, |root| {
        let (doc, _) = current(root, item, ceiling)?;
        let l = ledger(root, item)?;
        let version = l.version();
        if let Some(expected) = expected_version {
            anyhow::ensure!(expected == version, "stale_reader_passes");
        }
        let mut cache = BTreeMap::new();
        let visible = ranked(l.passes.iter().filter(|p| authorized(root, item, p, &doc, ceiling, &mut cache)));
        let total = visible.len();
        let (mut passes, mut bytes, mut next) = (Vec::new(), 512, offset);
        for (index, p) in visible.iter().enumerate().skip(offset).take(limit) {
            let timing = totals(&timing(root, item, &p.pass_id)?);
            let listed = ListedPass {
                pass_id: p.pass_id.clone(),
                ordinal: p.ordinal,
                display_rank: index + 1,
                kind: p.kind,
                status: p.status,
                method: p.method,
                start_date: p.start_date.clone(),
                end_date: p.end_date.clone(),
                estimated_duration_minutes: p.estimated_duration_minutes,
                duration_display_unit: p.duration_display_unit,
                timezone: p.timezone.clone(),
                revision_basis: p.revision_basis,
                revision_id: p.revision_id.clone(),
                canonical_sha256: p.canonical_sha256.clone(),
                // A pass of a same-body revision (only metadata edited since) is current.
                source_status: p.revision_id.as_ref().map(|r| {
                    if p.canonical_sha256.as_deref() == Some(&doc.canonical_sha256)
                        && crate::body::same_body(root, item, &doc.revision_id, r)
                    { "current" } else { "changed" }.into()
                }),
                lengths: p.lengths.clone(),
                created_at_ms: p.created_at_ms,
                completed_at_ms: p.completed_at_ms,
                rates: rates(p, timing.unique_ms),
                timing,
            };
            let size = serde_json::to_vec(&listed)?.len() + 1;
            if !passes.is_empty() && bytes + size > MAX_PAGE_BYTES {
                break;
            }
            bytes += size;
            passes.push(listed);
            next += 1;
        }
        Ok(PassPage {
            item_id: item.into(),
            version,
            current_revision_id: doc.revision_id.clone(),
            passes,
            total,
            next_offset: (next < total).then_some(next),
            pass_limit: MAX_PASSES,
        })
    })
}

/// Per-day measured totals of one pass, paged and bound to its timing version.
pub fn stats(
    library: &Path,
    item: &str,
    pass_id: &str,
    offset: usize,
    limit: usize,
    expected_timing_version: Option<&str>,
    ceiling: Sensitivity,
) -> Result<DayPage> {
    anyhow::ensure!(
        valid_item_id(item) && valid_pass_id(pass_id) && (1..=400).contains(&limit)
            && (offset == 0 || expected_timing_version.is_some()),
        "invalid_reader_pass"
    );
    with_consistent_snapshot(library, |root| {
        let (doc, _) = current(root, item, ceiling)?;
        let l = ledger(root, item)?;
        let p = l.passes.iter().find(|p| p.pass_id == pass_id).context("reader_pass_not_found")?;
        anyhow::ensure!(authorized(root, item, p, &doc, ceiling, &mut BTreeMap::new()), "reader_pass_not_found");
        let t = timing(root, item, pass_id)?;
        let timing_version = t.version();
        if let Some(expected) = expected_timing_version {
            anyhow::ensure!(expected == timing_version, "stale_reader_pass_timing");
        }
        let all = days(&t, &p.timezone)?;
        let total = all.len();
        let days: Vec<DayTotal> = all.into_iter().skip(offset).take(limit).collect();
        let next = offset + days.len();
        Ok(DayPage {
            item_id: item.into(),
            pass_id: pass_id.into(),
            timezone: p.timezone.clone(),
            timing_version,
            days,
            total,
            next_offset: (next < total).then_some(next),
        })
    })
}

fn change_pass_id(change: &Change) -> &str {
    match change {
        Change::CreateTracked { pass_id, .. }
        | Change::CreateRetrospective { pass_id, .. }
        | Change::Update { pass_id, .. }
        | Change::Complete { pass_id, .. }
        | Change::Reopen { pass_id }
        | Change::Delete { pass_id, .. } => pass_id,
    }
}

pub fn edit(
    library: &Path,
    item: &str,
    expected_version: &str,
    change: Change,
    principal: &str,
    ceiling: Sensitivity,
    now_utc_ms: i64,
) -> Result<EditResult> {
    let timing_ok = match &change {
        Change::Complete { expected_timing_version: v, .. } | Change::Delete { expected_timing_version: v, .. } => {
            v.as_deref().is_none_or(valid_lower_hex_64)
        }
        _ => true,
    };
    anyhow::ensure!(
        valid_item_id(item)
            && valid_lower_hex_64(expected_version)
            && valid_pass_id(change_pass_id(&change))
            && timing_ok
            && !principal.is_empty()
            && principal.len() <= 128,
        "invalid_reader_pass"
    );
    let change_sha256 = reader_sha256(&serde_json::to_vec(&change)?);
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (doc, bodies) = current(&root, item, ceiling)?;
    let mut l = ledger(&root, item)?;
    if l.version() != expected_version {
        // Only the immediately preceding exact attempt is replayed; older ones are stale.
        if let Some(r) = &l.last {
            if r.principal == principal && r.expected_version == expected_version && r.change_sha256 == change_sha256 {
                return Ok(r.result.clone());
            }
        }
        anyhow::bail!("stale_reader_passes")
    }
    let id = change_pass_id(&change).to_owned();
    let find = |l: &Ledger| -> Result<usize> {
        if l.tombstones.iter().any(|t| t.pass_id == id) {
            anyhow::bail!("reader_pass_not_found")
        }
        l.passes.iter().position(|p| p.pass_id == id).context("reader_pass_not_found")
    };
    let mut cache = BTreeMap::new();
    let check_timing = |root: &Path, expected: &Option<String>| -> Result<()> {
        if let Some(expected) = expected {
            anyhow::ensure!(timing(root, item, &id)?.version() == *expected, "stale_reader_pass_timing");
        }
        Ok(())
    };
    let mut remove_timing = false;
    let (ordinal, outcome) = match change {
        Change::CreateTracked { revision_id, method, start_date, timezone, .. } => {
            anyhow::ensure!(valid_revision_id(&revision_id) && valid_timezone(&timezone), "invalid_reader_pass");
            validate_fields(&Editable { kind: Kind::Tracked, status: Status::InProgress, start: &start_date, end: &None, estimate: None, unit: None })?;
            anyhow::ensure!(revision_id == doc.revision_id, "stale_item_revision");
            let ordinal = admit_new(&l, &id)?;
            l.passes.push(Pass {
                pass_id: id.clone(), ordinal, kind: Kind::Tracked, status: Status::InProgress, method, start_date,
                end_date: None, estimated_duration_minutes: None, duration_display_unit: None, timezone,
                revision_basis: Some(RevisionBasis::StartedOnRevision), revision_id: Some(doc.revision_id.clone()),
                canonical_sha256: Some(doc.canonical_sha256.clone()), lengths: Some(lengths(&doc, &bodies)),
                created_at_ms: now_utc_ms, completed_at_ms: None,
            });
            l.last_ordinal = ordinal;
            (ordinal, "created")
        }
        Change::CreateRetrospective {
            method, start_date, end_date, estimated_duration_minutes, duration_display_unit, timezone, reference_revision_id, ..
        } => {
            anyhow::ensure!(
                valid_timezone(&timezone) && reference_revision_id.as_deref().is_none_or(valid_revision_id),
                "invalid_reader_pass"
            );
            validate_fields(&Editable {
                kind: Kind::Retrospective, status: Status::Completed, start: &start_date, end: &end_date,
                estimate: estimated_duration_minutes, unit: duration_display_unit,
            })?;
            // A calculation reference is only ever the then-current revision, never a
            // historical one picked by date.
            if let Some(r) = &reference_revision_id {
                anyhow::ensure!(*r == doc.revision_id, "stale_item_revision");
            }
            let ordinal = admit_new(&l, &id)?;
            let reference = reference_revision_id.is_some();
            l.passes.push(Pass {
                pass_id: id.clone(), ordinal, kind: Kind::Retrospective, status: Status::Completed, method, start_date,
                end_date, estimated_duration_minutes, duration_display_unit, timezone,
                revision_basis: reference.then_some(RevisionBasis::ReferenceAtEntry),
                revision_id: reference.then(|| doc.revision_id.clone()),
                canonical_sha256: reference.then(|| doc.canonical_sha256.clone()),
                lengths: reference.then(|| lengths(&doc, &bodies)),
                created_at_ms: now_utc_ms, completed_at_ms: Some(now_utc_ms),
            });
            l.last_ordinal = ordinal;
            (ordinal, "created")
        }
        Change::Update { method, start_date, end_date, estimated_duration_minutes, duration_display_unit, .. } => {
            let i = find(&l)?;
            anyhow::ensure!(authorized(&root, item, &l.passes[i], &doc, ceiling, &mut cache), "reader_pass_not_found");
            let p = &mut l.passes[i];
            validate_fields(&Editable {
                kind: p.kind, status: p.status, start: &start_date, end: &end_date,
                estimate: estimated_duration_minutes, unit: duration_display_unit,
            })?;
            p.method = method;
            p.start_date = start_date;
            p.end_date = end_date;
            p.estimated_duration_minutes = estimated_duration_minutes;
            p.duration_display_unit = duration_display_unit;
            (p.ordinal, "updated")
        }
        Change::Complete { end_date, expected_timing_version, .. } => {
            let i = find(&l)?;
            anyhow::ensure!(authorized(&root, item, &l.passes[i], &doc, ceiling, &mut cache), "reader_pass_not_found");
            anyhow::ensure!(l.passes[i].status == Status::InProgress, "reader_pass_conflict");
            let p = &l.passes[i];
            anyhow::ensure!(p.kind == Kind::Retrospective || end_date.is_some(), "invalid_reader_pass");
            validate_fields(&Editable {
                kind: p.kind, status: Status::Completed, start: &p.start_date, end: &end_date,
                estimate: p.estimated_duration_minutes, unit: p.duration_display_unit,
            })?;
            check_timing(&root, &expected_timing_version)?;
            let p = &mut l.passes[i];
            p.status = Status::Completed;
            p.end_date = end_date;
            p.completed_at_ms = Some(now_utc_ms);
            (p.ordinal, "completed")
        }
        Change::Reopen { .. } => {
            let i = find(&l)?;
            anyhow::ensure!(authorized(&root, item, &l.passes[i], &doc, ceiling, &mut cache), "reader_pass_not_found");
            let p = &mut l.passes[i];
            anyhow::ensure!(p.status == Status::Completed, "reader_pass_conflict");
            // In-progress has no end date; recorded time is untouched.
            p.status = Status::InProgress;
            p.end_date = None;
            p.completed_at_ms = None;
            (p.ordinal, "reopened")
        }
        Change::Delete { expected_timing_version, .. } => {
            let i = find(&l)?;
            anyhow::ensure!(authorized(&root, item, &l.passes[i], &doc, ceiling, &mut cache), "reader_pass_not_found");
            check_timing(&root, &expected_timing_version)?;
            let p = l.passes.remove(i);
            l.tombstones.push(Tombstone { pass_id: p.pass_id, ordinal: p.ordinal });
            remove_timing = true;
            (p.ordinal, "deleted")
        }
    };
    l.sequence += 1;
    let result = EditResult { item_id: item.into(), version: l.version(), pass_id: id.clone(), ordinal, outcome: outcome.into() };
    l.last = Some(Receipt {
        principal: principal.into(),
        expected_version: expected_version.into(),
        change_sha256,
        result: result.clone(),
    });
    let bytes = serde_json::to_vec(&l)?;
    let limit = if outcome == "created" { MAX_LEDGER_BYTES } else { MAX_LEDGER_FILE_BYTES };
    anyhow::ensure!(bytes.len() <= limit, "reader_pass_limit_exceeded");
    ensure_contained_dir(&root, Path::new("catalog/reader-passes"))?;
    durable_replace(&root, &ledger_path(&root, item), &bytes)?;
    if remove_timing {
        // The tombstone is already durable, so a leftover file is unreachable data.
        let path = timing_path(&root, item, &id);
        if fs::symlink_metadata(&path).is_ok() {
            fs::remove_file(&path)?;
            sync_dir(&root.join(timing_dir(item)))?;
        }
    }
    lock.unlock()?;
    Ok(result)
}

fn admit_new(l: &Ledger, id: &str) -> Result<u64> {
    anyhow::ensure!(
        !l.passes.iter().any(|p| p.pass_id == id) && !l.tombstones.iter().any(|t| t.pass_id == id),
        "reader_pass_conflict"
    );
    anyhow::ensure!(
        l.passes.len() < MAX_PASSES && l.passes.len() + l.tombstones.len() < MAX_PASS_IDENTITIES,
        "reader_pass_limit_exceeded"
    );
    Ok(l.last_ordinal + 1)
}

#[derive(Debug, Clone)]
struct Session {
    session_id: String,
    root: PathBuf,
    item: String,
    pass: String,
    revision: String,
    principal: String,
    boot: String,
    origin_utc: i64,
    origin_mono: i64,
}

static SESSIONS: std::sync::OnceLock<std::sync::Mutex<Vec<Session>>> = std::sync::OnceLock::new();
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn sessions() -> Result<std::sync::MutexGuard<'static, Vec<Session>>> {
    SESSIONS.get_or_init(Default::default).lock().map_err(|_| anyhow::anyhow!("library_query_failed"))
}

fn live(s: &Session, clock: &TimingClock) -> bool {
    s.boot == clock.boot_id && clock.mono_ms >= s.origin_mono
        && clock.mono_ms - s.origin_mono <= SESSION_WINDOW_MS + REPORT_GRACE_MS
}

/// Starts a ≤300000 ms server-issued timing session for one tracked, in-progress pass
/// on the current revision. Not durable: a daemon restart (new boot) invalidates it.
pub fn timing_start(
    library: &Path,
    item: &str,
    pass_id: &str,
    principal: &str,
    ceiling: Sensitivity,
    clock: &TimingClock,
) -> Result<TimingSession> {
    anyhow::ensure!(
        valid_item_id(item) && valid_pass_id(pass_id) && !principal.is_empty() && principal.len() <= 128,
        "invalid_reader_pass"
    );
    let root = existing_canonical_root(library)?;
    let revision = with_consistent_snapshot(&root, |root| {
        let (doc, _) = current(root, item, ceiling)?;
        let l = ledger(root, item)?;
        anyhow::ensure!(!l.tombstones.iter().any(|t| t.pass_id == pass_id), "reader_pass_not_found");
        let p = l.passes.iter().find(|p| p.pass_id == pass_id).context("reader_pass_not_found")?;
        anyhow::ensure!(authorized(root, item, p, &doc, ceiling, &mut BTreeMap::new()), "reader_pass_not_found");
        anyhow::ensure!(p.kind == Kind::Tracked, "reader_pass_not_timeable");
        anyhow::ensure!(p.status == Status::InProgress, "reader_pass_closed");
        anyhow::ensure!(same_body_pass(root, item, p, &doc), "stale_item_revision");
        anyhow::ensure!(timing(root, item, pass_id)?.segments.len() < MAX_SEGMENTS, "reader_pass_limit_exceeded");
        Ok(doc.revision_id.clone())
    })?;
    let mut table = sessions()?;
    // Only this boot's expired sessions are pruned; other boots never coexist in production.
    table.retain(|s| s.boot != clock.boot_id || live(s, clock));
    anyhow::ensure!(table.len() < MAX_SESSIONS, "reader_pass_limit_exceeded");
    let n = SESSION_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let session_id = format!(
        "tsess_{}",
        &reader_sha256(format!("{}\0{}\0{n}\0{}\0{}\0{item}\0{pass_id}", clock.boot_id, root.display(), clock.utc_ms, clock.mono_ms).as_bytes())[..32]
    );
    table.push(Session {
        session_id: session_id.clone(), root, item: item.into(), pass: pass_id.into(), revision: revision.clone(),
        principal: principal.into(), boot: clock.boot_id.clone(), origin_utc: clock.utc_ms, origin_mono: clock.mono_ms,
    });
    Ok(TimingSession {
        session_id,
        item_id: item.into(),
        pass_id: pass_id.into(),
        revision_id: revision,
        origin_utc_ms: clock.utc_ms,
        window_ms: SESSION_WINDOW_MS,
        max_segment_ms: MAX_SEGMENT_MS,
        report_deadline_utc_ms: clock.utc_ms + SESSION_WINDOW_MS + REPORT_GRACE_MS,
    })
}

fn payload_sha256(session_id: &str, s: &SegmentReport) -> Result<String> {
    Ok(reader_sha256(&serde_json::to_vec(&(session_id, s))?))
}

/// Appends device-reported active intervals. Exact replays are no-ops (even after the
/// session expired, the daemon restarted or the pass completed); a changed payload
/// under an accepted ID conflicts; new intervals need the live coherent session.
/// The whole report is accepted or rejected; nothing is clamped.
#[allow(clippy::too_many_arguments)]
pub fn timing_report(
    library: &Path,
    item: &str,
    pass_id: &str,
    session_id: &str,
    segments: &[SegmentReport],
    principal: &str,
    ceiling: Sensitivity,
    clock: &TimingClock,
) -> Result<ReportResult> {
    let mut ids = BTreeSet::new();
    anyhow::ensure!(
        valid_item_id(item)
            && valid_pass_id(pass_id)
            && session_id.len() <= 64
            && safe_component(session_id, "tsess_")
            && !principal.is_empty()
            && principal.len() <= 128
            && (1..=MAX_REPORT_SEGMENTS).contains(&segments.len())
            && serde_json::to_vec(segments)?.len() <= MAX_REPORT_BYTES
            && segments.iter().all(|s| valid_segment_id(&s.segment_id) && ids.insert(s.segment_id.as_str())),
        "invalid_reader_pass_timing"
    );
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (doc, _) = current(&root, item, ceiling)?;
    let l = ledger(&root, item)?;
    // Deleted passes reveal nothing and are never recreated by delayed packets.
    anyhow::ensure!(!l.tombstones.iter().any(|t| t.pass_id == pass_id), "reader_pass_closed");
    let p = l.passes.iter().find(|p| p.pass_id == pass_id).context("reader_pass_not_found")?;
    anyhow::ensure!(authorized(&root, item, p, &doc, ceiling, &mut BTreeMap::new()), "reader_pass_not_found");
    anyhow::ensure!(p.kind == Kind::Tracked, "reader_pass_not_timeable");
    let mut t = timing(&root, item, pass_id)?;
    let index: BTreeMap<&str, &Accepted> = t
        .segments
        .iter()
        .filter(|a| a.session_id == session_id && a.principal == principal)
        .map(|a| (a.segment_id.as_str(), a))
        .collect();
    let mut outcomes = Vec::new();
    let mut fresh = Vec::new();
    for s in segments {
        let hash = payload_sha256(session_id, s)?;
        match index.get(s.segment_id.as_str()) {
            Some(a) if a.payload_sha256 == hash => {
                outcomes.push(SegmentOutcome { segment_id: s.segment_id.clone(), outcome: "duplicate".into() })
            }
            Some(_) => anyhow::bail!("reader_pass_segment_conflict"),
            None => {
                outcomes.push(SegmentOutcome { segment_id: s.segment_id.clone(), outcome: "accepted".into() });
                fresh.push((s, hash));
            }
        }
    }
    if !fresh.is_empty() {
        anyhow::ensure!(p.status == Status::InProgress, "reader_pass_closed");
        anyhow::ensure!(same_body_pass(&root, item, p, &doc), "stale_item_revision");
        let session = {
            let mut table = sessions()?;
            let found = table
                .iter()
                .position(|s| s.session_id == session_id && s.root == root && s.item == item && s.pass == pass_id
                    && s.principal == principal
                    && crate::body::same_body(&root, item, &doc.revision_id, &s.revision))
                .context("reader_pass_session_expired")?;
            if !live(&table[found], clock) {
                table.remove(found);
                anyhow::bail!("reader_pass_session_expired")
            }
            let s = &table[found];
            let drift = (clock.utc_ms - s.origin_utc) - (clock.mono_ms - s.origin_mono);
            if drift.abs() > CLOCK_TOLERANCE_MS {
                // Backward/forward wall-clock steps or uncounted sleep: suspend this
                // session; time resumes only from a fresh coherent sample.
                table.remove(found);
                anyhow::bail!("reader_pass_clock_discontinuity")
            }
            s.clone()
        };
        let elapsed = clock.mono_ms - session.origin_mono;
        for (s, _) in &fresh {
            anyhow::ensure!(
                s.start_offset_ms >= 0
                    && s.start_offset_ms < s.end_offset_ms
                    && s.end_offset_ms - s.start_offset_ms <= MAX_SEGMENT_MS
                    && s.end_offset_ms <= SESSION_WINDOW_MS
                    && s.end_offset_ms <= elapsed,
                "invalid_reader_pass_timing"
            );
        }
        anyhow::ensure!(t.segments.len() + fresh.len() <= MAX_SEGMENTS, "reader_pass_limit_exceeded");
        for (s, hash) in fresh {
            t.sequence += 1;
            t.segments.push(Accepted {
                session_id: session_id.into(),
                principal: principal.into(),
                segment_id: s.segment_id.clone(),
                payload_sha256: hash,
                mode: s.mode,
                start_ms: session.origin_utc + s.start_offset_ms,
                end_ms: session.origin_utc + s.end_offset_ms,
                seq: t.sequence,
            });
        }
        let bytes = serde_json::to_vec(&t)?;
        anyhow::ensure!(bytes.len() <= MAX_TIMING_BYTES, "reader_pass_limit_exceeded");
        ensure_contained_dir(&root, &timing_dir(item))?;
        durable_replace(&root, &timing_path(&root, item, pass_id), &bytes)?;
    }
    lock.unlock()?;
    Ok(ReportResult {
        item_id: item.into(),
        pass_id: pass_id.into(),
        session_id: session_id.into(),
        outcomes,
        totals: totals(&t),
    })
}

#[cfg(test)]
mod tests;
