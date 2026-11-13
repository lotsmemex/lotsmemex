// Demo data source. Synthetic only — never talks to a network or the real library.
// Its async shape mirrors the owner-daemon contracts the private Mac gateway is expected to
// expose (library_query paging + view, item read, context_read byte segments, update_item_metadata
// with opaque revision CAS), so the UI is written against the real boundaries. Failures are
// thrown as { code } errors; `unavailable` means "not connected" and is never silently replaced.

import { buildLibrary, GUJI_SAMPLE, TALK_SAMPLE, TALK_EN, TALK_EN_ZH, bodyOf, summaryOf, minutesOf, longChineseTranscript, insightsOf, GROUPS, MARKDOWN_SAMPLE, termHighlights, DEMO_BOOK, DEMO_BOOK_REPAIRED, DEMO_ALIGN } from './fixtures.mjs';

const MAX_LIMIT = 200;
const MAX_SEGMENT = 16 * 1024;
// Demo chunks are small so following next_offset across one chapter is exercised.
const DEMO_CHUNK = 2048;
const enc = new TextEncoder();
const dec = new TextDecoder('utf-8', { fatal: true });
const fail = (code) => Object.assign(new Error(code), { code });
const delay = (ms) => new Promise((r) => setTimeout(r, ms));
const rev = (id, n) => `rev_${id}_${n.toString(36)}_${(n * 2654435761 >>> 0).toString(16)}`;

// ---------- synthetic cover PNGs (no network, no canvas): RGB, deflate stored blocks ----------
const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
const crc32 = (bytes) => { let c = 0xffffffff; for (const b of bytes) c = CRC_TABLE[(c ^ b) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
function pngOf(w, hgt, px) {
  const raw = new Uint8Array((w * 3 + 1) * hgt);
  for (let y = 0, at = 0; y < hgt; y += 1) { raw[at++] = 0; for (let x = 0; x < w; x += 1) { const [r, g, b] = px(x / w, y / hgt, w / hgt); raw[at++] = r; raw[at++] = g; raw[at++] = b; } }
  const blocks = Math.ceil(raw.length / 65535); const z = new Uint8Array(2 + raw.length + blocks * 5 + 4);
  z[0] = 0x78; z[1] = 0x01; let o = 2; let a = 1; let b2 = 0;
  for (let i = 0; i < raw.length; i += 65535) {
    const n = Math.min(65535, raw.length - i);
    z.set([i + n >= raw.length ? 1 : 0, n & 255, n >> 8, ~n & 255, (~n >> 8) & 255], o); o += 5; z.set(raw.subarray(i, i + n), o); o += n;
  }
  for (const v of raw) { a = (a + v) % 65521; b2 = (b2 + a) % 65521; }
  new DataView(z.buffer).setUint32(o, ((b2 << 16) | a) >>> 0);
  const chunk = (type, data) => {
    const c = new Uint8Array(12 + data.length); const dv = new DataView(c.buffer);
    dv.setUint32(0, data.length); c.set(enc.encode(type), 4); c.set(data, 8); dv.setUint32(8 + data.length, crc32(c.subarray(4, 8 + data.length))); return c;
  };
  const ihdr = new Uint8Array(13); const dv = new DataView(ihdr.buffer); dv.setUint32(0, w); dv.setUint32(4, hgt); ihdr.set([8, 2, 0, 0, 0], 8);
  const parts = [Uint8Array.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', z), chunk('IEND', new Uint8Array())];
  const out = new Uint8Array(parts.reduce((s, p) => s + p.length, 0)); let at = 0; for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
}
const hsl = (hh, s, l) => { const k = (n) => (n + hh / 30) % 12; const q = s * Math.min(l, 1 - l); return [0, 8, 4].map((n) => Math.round(255 * (l - q * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1))))); };
const mix = (p, q, t) => p.map((v, i) => Math.round(v + (q[i] - v) * t));
// A soft podcast-style artwork: warm two-tone wash, a glow, one thin ring and a row of waveform bars.
// `hue` picks the palette (rose / mauve / sage), so versions of one cover look different.
function coverPixel(hue) {
  const top = hsl(hue, 0.4, 0.91); const bottom = hsl(hue - 22, 0.3, 0.79); const deep = hsl(hue - 6, 0.22, 0.5); const cream = hsl(hue, 0.35, 0.98);
  return (u, v, ar) => {
    let c = mix(top, bottom, Math.min(1, Math.max(0, 0.65 * v + 0.35 * u)));
    const dx = (u - 0.68) * ar; const dy = v - 0.38; const d = Math.hypot(dx, dy);
    c = mix(c, cream, Math.max(0, 1 - d / 0.42) ** 2 * 0.85);
    c = mix(c, deep, 0.55 * Math.max(0, 1 - Math.abs(d - 0.27) / 0.0075)); // anti-aliased thin ring
    const bx = u * ar - 0.12; const n = Math.floor(bx / 0.055);
    if (bx > 0 && n < 9 && (bx % 0.055) < 0.026) { const len = 0.05 + 0.13 * Math.abs(Math.sin(n * 1.7 + hue)); if (Math.abs(v - 0.74) < len / 2) c = mix(c, deep, 0.7); }
    return c;
  };
}

export function createDemoSource() {
  const items = buildLibrary().map((it) => ({ ...it, revision_number: 1, revision_id: rev(it.id, 1) }));
  const byId = new Map(items.map((it) => [it.id, it]));
  // staleBook: the book's manifest no longer matches its canonical text (reader fails honestly).
  // Paragraph alignment (demo only): noAlignment / noRepair / readOnly change what session.info
  // reports; failNextJob makes the next admitted repair job fail; unknownNextAlignRequest admits the
  // next repair request but loses its reply; staleAlignOnce rejects the next later page once with
  // stale_chapter_alignment; corruptAlignment leaves a one-byte gap in the served rows.
  const flags = { offline: false, conflictNextSave: false, unknownNextSave: false, staleBook: false,
    noAlignment: false, noRepair: false, readOnly: false, failNextJob: false, unknownNextAlignRequest: false, staleAlignOnce: false, corruptAlignment: false,
    // 翻译本章 (demo only, all synthetic): noTranslation hides the capability; failNextTranslation /
    // unknownNextTranslation make the next admitted job fail / end outcome_unknown; lostNextTranslateReply
    // admits the next request but loses its reply.
    noTranslation: false, failNextTranslation: false, unknownNextTranslation: false, lostNextTranslateReply: false,
    // busyPrefetch (demo only): a background (prefetch) chunk read meets a busy Mac (library_loading).
    busyPrefetch: false,
    // 编辑 Markdown / 对比修改 (demo only): noEdit / noRevisionText hide the capabilities;
    // lostNextEditChunk / lostNextEditCommit apply the next chunk / commit but lose its reply;
    // staleNextEdit moves the book to a new revision (as an edit elsewhere) before the next begin.
    noEdit: false, noRevisionText: false, lostNextEditChunk: false, lostNextEditCommit: false, staleNextEdit: false,
    // 格式整理 (demo only, all synthetic, no model): noFormat / noFormatBatch hide the capabilities;
    // failNextFormat / unknownNextFormat make the next target fail / end outcome_unknown;
    // lostNextFormatReply / lostNextRestoreReply apply the request / restore but lose the reply;
    // privateFormat marks every target Private (the confirmation asks for consent); denyBatch
    // refuses whole-batch reads and resume (as when a member was trashed) for the listed item ids.
    noFormat: false, noFormatBatch: false, failNextFormat: false, unknownNextFormat: false, lostNextFormatReply: false, lostNextRestoreReply: false,
    privateFormat: false, denyBatch: [],
    // AI 章译文 discovery / correspondence (demo only): noTranslationMetadata / noTranslationAlignment
    // hide the capabilities; legacyTranslation makes every completed projection report status=missing.
    noTranslationMetadata: false, noTranslationAlignment: false, legacyTranslation: false,
    // 长文阅读 (demo only, synthetic): noStateSync hides reader_state_sync_v1 (local-only position);
    // lostNextStateSave applies the next save but loses its reply; foreignNextStateSave makes another
    // device save first (the next save meets stale_reader_state); annotations advertises
    // reader_annotations_v1 (off by default: marks stay on this page only); lostNextAnnEdit applies the
    // next mark edit but loses its reply; failAfterNextAnnCommit commits it then answers internal_error
    // (sync failed after the rename); foreignNextAnnEdit lets another device edit first (stale);
    // annPageSize makes list pages smaller (paging).
    // 译文标记 (reader_annotations_ai_translation_v1, synthetic): noAiAnnotations hides the capability (a
    // pre-AI daemon: AI requests fail invalid_request); lostNextAiAnnEdit applies the next AI mark edit but
    // loses its reply; droppedNextAiAnnEdit loses the next one before it applies (also unknown);
    // aiAnnPageSize makes AI list pages smaller (paging with expected_view_version).
    // 说话人名字 (synthetic): talkEnVariant changes demo-talk-en's translation — 'merged' joins two blocks
    // (no one-to-one correspondence), 'reordered' swaps two turns per part (same block count).
    talkEnVariant: null,
    noAiAnnotations: false, lostNextAiAnnEdit: false, droppedNextAiAnnEdit: false, aiAnnPageSize: 0,
    noStateSync: false, lostNextStateSave: false, foreignNextStateSave: false, annotations: false, lostNextAnnEdit: false, failAfterNextAnnCommit: false, foreignNextAnnEdit: false, annPageSize: 0,
    // 阅读记录 (reader_passes_v1, synthetic): noPasses hides the capability; lostNextPassEdit applies the
    // next edit but loses its reply (unknown); droppedNextPassEdit loses the next edit before it applies
    // (also unknown); foreignNextPassEdit lets another device edit first (stale_reader_passes);
    // expireNextSession expires the live timing session before the next report; lostNextReport accepts
    // the next report but loses its reply; failReports refuses every report with unknown (待同步 stays).
    noPasses: false, lostNextPassEdit: false, droppedNextPassEdit: false, foreignNextPassEdit: false, expireNextSession: false, lostNextReport: false, failReports: false,
    // 条目封面 (item_cover_v1, synthetic PNGs drawn by code): noCover hides the capability (any cover
    // request then fails invalid_request); coverNull makes every batch position null; staleCoverReads
    // is how many cover reads see the version change after their first chunk (the next chunk meets
    // stale_cover_version; the new version has other bytes); coverCorrupt 'hash' flips one served byte,
    // 'total' serves one byte fewer (total_bytes ≠ byte_size), both under the descriptor's sha256.
    noCover: false, coverNull: false, staleCoverReads: 0, coverCorrupt: null,
    // 当前分组 (ui_active_group_v1, synthetic): noActiveGroup hides the capability (requests fail
    // invalid_request); lostActiveGroupReplies applies that many next requests but loses their replies.
    noActiveGroup: false, lostActiveGroupReplies: 0,
    // Item translation status (demo only, synthetic) per item id: 'error:<code>' fails the status read
    // with that code; 'running03' reports running 0/3; 'unavailable' has no reason; 'bump' moves the item
    // to a new revision on its next status read (as an edit on the Mac while the phone shows the older
    // one), then reports running 0/3; 'ahead' always names a revision the item read never returns.
    // trStatusReads counts status reads (polling is visible in tests).
    trStatus: {}, trStatusReads: 0 };
  // Ordinary documents (reader_v1 side `source`): heading-based chapters `section-N` partitioning the
  // body (text before the first heading is section-0; no heading → one full-text section-0).
  const srcSha = (it) => `demo${String(it.revision_number).padStart(4, '0')}${[...enc.encode(original(it))].reduce((h, b) => (h * 31 + b) >>> 0, 7).toString(16).padStart(8, '0')}`.padEnd(64, '0');
  function sourceChapters(it) {
    const md = original(it); const bytes = enc.encode(md).length;
    const heads = [];
    let off = 0;
    for (const line of md.split('\n')) {
      const m = /^(#{1,6})[ \t]+(.*)$/.exec(line);
      if (m) heads.push({ start: enc.encode(md.slice(0, off)).length, level: m[1].length, title: m[2].trim() });
      off += line.length + 1;
    }
    const cut = heads.filter((x) => x.start > 0 || heads.length);
    const starts = [...(cut.length && cut[0].start === 0 ? [] : [{ start: 0, level: 1, title: it.title }]), ...cut];
    return starts.map((x, i) => ({ key: `section-${i}`, title: x.title, level: x.level, start: x.start, end: i + 1 < starts.length ? starts[i + 1].start : bytes, counterpart: null }));
  }
  // B2 reader state (memex.library.reader.state / save / resume, synthetic): per-item CAS on a version
  // that is the SHA-256 of the state's JSON (field order visit_id, position, bookmarks) as on the Mac.
  const readerStates = new Map(); let latestItem = null;
  const stateVersion = async (st) => [...new Uint8Array(await crypto.subtle.digest('SHA-256', enc.encode(JSON.stringify(st))))].map((b) => b.toString(16).padStart(2, '0')).join('');
  const EMPTY_STATE = () => ({ visit_id: '', position: null, bookmarks: [] });
  // 手工标记 ledgers (Manual reader annotations v1, synthetic): versions never repeat (a counter, no ABA).
  const ANN_STYLES = ['bold', 'red', 'blue', 'underline', 'yellow'];
  const annLedgers = new Map(); let annSeq = 0;
  const nextAnnVersion = () => `annv_${(annSeq += 1).toString(36).padStart(6, '0')}`;
  const ledger = (id) => { if (!annLedgers.has(id)) annLedgers.set(id, { version: nextAnnVersion(), marks: new Map(), receipt: null }); return annLedgers.get(id); };
  // 阅读记录 (reader_passes_v1, synthetic): the same semantics as the Mac's ledger — per-item CAS
  // version, last-exact-attempt receipt, never reused ordinals, display_rank by the entered start date,
  // timing sessions of 300000 ms on a monotonic clock (setPassClock replaces it in tests), whole-report
  // accept/reject, exact replay = duplicate, union of intervals in acceptance order, local-day split.
  function passesApi() {
    const books = new Map(); let pv = 0; let sv = 0;
    const hexv = (tag) => `${tag}${(pv += 1).toString(16)}`.padStart(64, '0').replace(/[^0-9a-f]/g, '0');
    const book = (id) => { if (!books.has(id)) books.set(id, { version: hexv('a'), lastOrd: 0, passes: [], tomb: new Set(), last: null }); return books.get(id); };
    const timings = new Map(); // pass_id → { version, seq, segs: [{key, sid, id, hash, mode, a, b, seq}] }
    const timingOf = (pid) => { if (!timings.has(pid)) timings.set(pid, { version: hexv('b'), seq: 0, segs: [] }); return timings.get(pid); };
    const sessions = new Map();
    const METHODS = [null, 'full', 'sampled', 'structural'];
    const okDate = (d) => {
      if (d === null) return true;
      if (!d || typeof d !== 'object' || !['year', 'month', 'day'].includes(d.precision) || typeof d.value !== 'string') return false;
      const m = { year: /^(\d{4})$/, month: /^(\d{4})-(\d{2})$/, day: /^(\d{4})-(\d{2})-(\d{2})$/ }[d.precision].exec(d.value);
      if (!m || Number(m[1]) < 1) return false;
      if (m[2] && (Number(m[2]) < 1 || Number(m[2]) > 12)) return false;
      if (m[3] && (Number(m[3]) < 1 || Number(m[3]) > new Date(Date.UTC(Number(m[1]), Number(m[2]), 0)).getUTCDate())) return false;
      return true;
    };
    const lo = (d) => { const [y, m = 1, dd = 1] = d.value.split('-').map(Number); return y * 10000 + m * 100 + dd; };
    const hi = (d) => { const [y, m = 12, dd] = d.value.split('-').map(Number); return y * 10000 + m * 100 + (dd || new Date(Date.UTC(y, m, 0)).getUTCDate()); };
    const fields = (kind, status, start, end, est, unit) => {
      const estOk = kind === 'tracked' ? est === null && unit === null : (est === null || (Number.isInteger(est) && est >= 1 && est <= 525600)) && (unit === null || ((unit === 'minutes' || unit === 'hours') && est !== null));
      if (!okDate(start) || !okDate(end) || !estOk || (status === 'in_progress' && end !== null)) throw fail('invalid_reader_pass');
      if (start && end && hi(end) < lo(start)) throw fail('invalid_reader_pass_dates');
    };
    const plain = (s) => s.replace(/^#{1,6}\s+/gm, '').replace(/[*_>`]/g, '');
    const lengthOf = (side, text) => ({ side, sha256: `demo${text.length}`.padEnd(64, '0').replace(/[^0-9a-f]/g, '0'),
      words: (plain(text).match(/[A-Za-z0-9]+(?:['’][A-Za-z]+)*/g) || []).length, characters: (text.match(/\p{Script=Han}/gu) || []).length });
    const lengths = (it) => ({ policy: 'memex-pass-count-v1', source: lengthOf('source', original(it)),
      translation: !it.book && it.translation === 'done' && !it.longZh && !it.guji && !it.talk && !it.markdown ? lengthOf('translation', doc(it, it.english ? 'zh' : 'en')) : null });
    const owned = (segs) => {
      const done = []; const out = [];
      for (const s of [...segs].sort((x, y) => x.seq - y.seq)) {
        let parts = [[s.a, s.b]];
        for (const [a, b] of done) parts = parts.flatMap(([x, y]) => (b <= x || a >= y ? [[x, y]] : [[x, Math.min(y, a)], [Math.max(x, b), y]].filter(([p, q]) => q > p)));
        for (const [x, y] of parts) out.push([x, y, s.mode]);
        done.push([s.a, s.b]);
      }
      return out;
    };
    const totals = (pid) => {
      const t = timingOf(pid); const m = { source: 0, bilingual: 0, translation: 0 };
      for (const [a, b, mode] of owned(t.segs)) m[mode] += b - a;
      const accepted = t.segs.reduce((n, s) => n + s.b - s.a, 0); const unique = m.source + m.bilingual + m.translation;
      return { timing_version: t.version, accepted_ms: accepted, unique_ms: unique, overlap_excluded_ms: accepted - unique, source_ms: m.source, bilingual_ms: m.bilingual,
        translation_ms: m.translation, segment_count: t.segs.length, segment_limit: 65536, recording_paused: false };
    };
    const rates = (p, unique) => {
      const minutes = p.kind === 'tracked' ? (unique > 0 ? unique / 60000 : null) : p.estimated_duration_minutes;
      const per = (w, c) => ({ words_per_minute: minutes && w ? w / minutes : null, characters_per_minute: minutes && c ? c / minutes : null });
      const s = p.lengths?.source; const t = p.lengths?.translation;
      return { basis: p.kind === 'tracked' ? 'measured' : 'retrospective_estimate', minutes, estimate: p.kind === 'retrospective' || p.method !== 'full' || p.status !== 'completed',
        equivalent_full_text: p.method !== 'full', source: per(s?.words, s?.characters), translation: per(t?.words, t?.characters),
        combined: per(s && t ? s.words + t.words : null, s && t ? s.characters + t.characters : null) };
    };
    const ranked = (B) => [...B.passes].sort((a, b) => (a.start_date === null) - (b.start_date === null)
      || (a.start_date && b.start_date && (a.start_date.value < b.start_date.value ? -1 : a.start_date.value > b.start_date.value ? 1 : 0)) || a.ordinal - b.ordinal || (a.pass_id < b.pass_id ? -1 : 1));
    const dayOf = (ms, tz) => new Intl.DateTimeFormat('en-CA', { timeZone: tz, year: 'numeric', month: '2-digit', day: '2-digit' }).format(new Date(ms));
    let clk = () => performance.now();
    const api = {
      setPassClock: (fn) => { clk = fn; },
      passesOf: (id) => structuredClone(book(id).passes),
      async passesList({ item_id, offset, limit, expected_version }) {
        await guard(); calls.push(`passesList:${offset}`);
        const it = byId.get(item_id);
        if (!it) throw fail('library_resource_not_found');
        if (!(Number.isInteger(limit) && limit >= 1 && limit <= 100) || !Number.isInteger(offset) || offset < 0 || (expected_version === null && offset !== 0)) throw fail('invalid_reader_pass');
        const B = book(item_id);
        if (expected_version !== null && expected_version !== B.version) throw fail('stale_reader_passes');
        const all = ranked(B);
        const passes = all.slice(offset, offset + limit).map((p, i) => {
          const timing = totals(p.pass_id);
          return { ...structuredClone(p), display_rank: offset + i + 1, source_status: p.revision_id ? (p.revision_id === it.revision_id ? 'current' : 'changed') : null, timing, rates: rates(p, timing.unique_ms) };
        });
        return { item_id, version: B.version, current_revision_id: it.revision_id, passes, total: all.length, next_offset: offset + limit < all.length ? offset + limit : null, pass_limit: 4096 };
      },
      async passesStats({ item_id, pass_id, offset, limit, expected_timing_version }) {
        await guard(); calls.push(`passesStats:${pass_id}`);
        const p = book(item_id).passes.find((x) => x.pass_id === pass_id);
        if (!byId.has(item_id)) throw fail('library_resource_not_found');
        if (!p) throw fail('reader_pass_not_found');
        const t = timingOf(pass_id);
        if (expected_timing_version !== null && expected_timing_version !== t.version) throw fail('stale_reader_pass_timing');
        const by = new Map();
        for (const [a, b, mode] of owned(t.segs)) {
          let c = a;
          while (c < b) {
            // the first millisecond of the next local day (or b): binary search, exact at midnight
            const day = dayOf(c, p.timezone); let end = b;
            if (dayOf(b - 1, p.timezone) !== day) { let x = c; let y = b - 1; while (y - x > 1) { const m = Math.floor((x + y) / 2); if (dayOf(m, p.timezone) === day) x = m; else y = m; } end = y; }
            const d = by.get(day) || { date: day, ms: 0, source_ms: 0, bilingual_ms: 0, translation_ms: 0 };
            d.ms += end - c; d[`${mode}_ms`] += end - c; by.set(day, d); c = end;
          }
        }
        const days = [...by.values()].sort((x, y) => (x.date < y.date ? -1 : 1));
        return { item_id, pass_id, timezone: p.timezone, timing_version: t.version, days: days.slice(offset, offset + limit), total: days.length, next_offset: offset + limit < days.length ? offset + limit : null };
      },
      async passesEdit({ item_id, expected_version, change }) {
        await guard(); calls.push(`passesEdit:${change?.op}`);
        const it = byId.get(item_id);
        if (!it) throw fail('library_resource_not_found');
        const B = book(item_id);
        if (flags.droppedNextPassEdit) { flags.droppedNextPassEdit = false; throw fail('unknown'); }
        const key = JSON.stringify([expected_version, change]);
        if (B.version !== expected_version && B.last?.key === key) return structuredClone(B.last.result);
        if (flags.foreignNextPassEdit) {
          flags.foreignNextPassEdit = false;
          B.lastOrd += 1;
          B.passes.push({ pass_id: `pass_other_${B.lastOrd}`, ordinal: B.lastOrd, kind: 'retrospective', status: 'completed', method: 'sampled', start_date: { value: '2023', precision: 'year' }, end_date: null,
            estimated_duration_minutes: 45, duration_display_unit: 'minutes', timezone: 'Asia/Shanghai', revision_basis: null, revision_id: null, canonical_sha256: null, lengths: null, created_at_ms: Date.now(), completed_at_ms: Date.now() });
          B.version = hexv('a'); B.last = null;
        }
        if (B.version !== expected_version) throw fail('stale_reader_passes');
        const c = change || {};
        if (typeof c.pass_id !== 'string' || !/^pass_[A-Za-z0-9_-]+$/.test(c.pass_id) || !METHODS.includes(c.method === undefined ? null : c.method)) throw fail('invalid_reader_pass');
        const find = () => { if (B.tomb.has(c.pass_id)) throw fail('reader_pass_not_found'); const p = B.passes.find((x) => x.pass_id === c.pass_id); if (!p) throw fail('reader_pass_not_found'); return p; };
        const tv = (v) => { if (v !== null && v !== timingOf(c.pass_id).version) throw fail('stale_reader_pass_timing'); };
        const fresh = () => { if (B.tomb.has(c.pass_id) || B.passes.some((x) => x.pass_id === c.pass_id)) throw fail('reader_pass_conflict'); if (B.passes.length >= 4096) throw fail('reader_pass_limit_exceeded'); B.lastOrd += 1; return B.lastOrd; };
        let p; let outcome;
        if (c.op === 'create_tracked') {
          fields('tracked', 'in_progress', c.start_date, null, null, null);
          if (c.revision_id !== it.revision_id) throw fail('stale_item_revision');
          p = { pass_id: c.pass_id, ordinal: fresh(), kind: 'tracked', status: 'in_progress', method: c.method, start_date: c.start_date, end_date: null, estimated_duration_minutes: null,
            duration_display_unit: null, timezone: c.timezone, revision_basis: 'started_on_revision', revision_id: it.revision_id, canonical_sha256: srcSha(it), lengths: lengths(it), created_at_ms: Date.now(), completed_at_ms: null };
          B.passes.push(p); outcome = 'created';
        } else if (c.op === 'create_retrospective') {
          fields('retrospective', 'completed', c.start_date, c.end_date, c.estimated_duration_minutes, c.duration_display_unit);
          if (c.reference_revision_id !== null && c.reference_revision_id !== it.revision_id) throw fail('stale_item_revision');
          const ref = c.reference_revision_id !== null;
          p = { pass_id: c.pass_id, ordinal: fresh(), kind: 'retrospective', status: 'completed', method: c.method, start_date: c.start_date, end_date: c.end_date,
            estimated_duration_minutes: c.estimated_duration_minutes, duration_display_unit: c.duration_display_unit, timezone: c.timezone, revision_basis: ref ? 'reference_at_entry' : null,
            revision_id: ref ? it.revision_id : null, canonical_sha256: ref ? srcSha(it) : null, lengths: ref ? lengths(it) : null, created_at_ms: Date.now(), completed_at_ms: Date.now() };
          B.passes.push(p); outcome = 'created';
        } else if (c.op === 'update') {
          p = find(); fields(p.kind, p.status, c.start_date, c.end_date, c.estimated_duration_minutes, c.duration_display_unit);
          Object.assign(p, { method: c.method, start_date: c.start_date, end_date: c.end_date, estimated_duration_minutes: c.estimated_duration_minutes, duration_display_unit: c.duration_display_unit });
          outcome = 'updated';
        } else if (c.op === 'complete') {
          p = find(); if (p.status !== 'in_progress') throw fail('reader_pass_conflict');
          if (p.kind === 'tracked' && !c.end_date) throw fail('invalid_reader_pass');
          fields(p.kind, 'completed', p.start_date, c.end_date, p.estimated_duration_minutes, p.duration_display_unit); tv(c.expected_timing_version);
          Object.assign(p, { status: 'completed', end_date: c.end_date, completed_at_ms: Date.now() }); outcome = 'completed';
        } else if (c.op === 'reopen') {
          p = find(); if (p.status !== 'completed') throw fail('reader_pass_conflict');
          Object.assign(p, { status: 'in_progress', end_date: null, completed_at_ms: null }); outcome = 'reopened';
        } else if (c.op === 'delete') {
          p = find(); tv(c.expected_timing_version);
          B.passes = B.passes.filter((x) => x !== p); B.tomb.add(p.pass_id); timings.delete(p.pass_id); outcome = 'deleted';
        } else throw fail('invalid_reader_pass');
        B.version = hexv('a');
        const result = { item_id, version: B.version, pass_id: c.pass_id, ordinal: p.ordinal, outcome };
        B.last = { key, result };
        if (flags.lostNextPassEdit) { flags.lostNextPassEdit = false; throw fail('unknown'); }
        return structuredClone(result);
      },
      async passesTimingStart({ item_id, pass_id }) {
        await guard(); calls.push('passesTimingStart');
        const it = byId.get(item_id);
        const p = it && book(item_id).passes.find((x) => x.pass_id === pass_id);
        if (!it) throw fail('library_resource_not_found');
        if (!p) throw fail('reader_pass_not_found');
        if (p.kind !== 'tracked') throw fail('reader_pass_not_timeable');
        if (p.status !== 'in_progress') throw fail('reader_pass_closed');
        if (p.revision_id !== it.revision_id) throw fail('stale_item_revision');
        const session_id = `tsess_demo${(sv += 1).toString(36)}`; const now = Date.now();
        sessions.set(session_id, { item_id, pass_id, mono: clk(), utc: now });
        return { session_id, item_id, pass_id, revision_id: it.revision_id, origin_utc_ms: now, window_ms: 300000, max_segment_ms: 300000, report_deadline_utc_ms: now + 360000 };
      },
      async passesTimingReport({ item_id, pass_id, session_id, segments }) {
        await guard(); calls.push(`passesTimingReport:${segments.length}`);
        if (flags.failReports) throw fail('unknown');
        const it = byId.get(item_id); const B = it && book(item_id);
        if (!it) throw fail('library_resource_not_found');
        if (!Array.isArray(segments) || !segments.length || segments.length > 64 || new Set(segments.map((s) => s.segment_id)).size !== segments.length
          || segments.some((s) => !['source', 'bilingual', 'translation'].includes(s.mode) || !Number.isSafeInteger(s.start_offset_ms) || !Number.isSafeInteger(s.end_offset_ms))) throw fail('invalid_reader_pass_timing');
        if (B.tomb.has(pass_id)) throw fail('reader_pass_closed');
        const p = B.passes.find((x) => x.pass_id === pass_id);
        if (!p) throw fail('reader_pass_not_found');
        if (p.kind !== 'tracked') throw fail('reader_pass_not_timeable');
        const t = timingOf(pass_id); const outcomes = []; const fresh = [];
        for (const s of segments) {
          const hash = JSON.stringify([s.mode, s.start_offset_ms, s.end_offset_ms]);
          const old = t.segs.find((x) => x.sid === session_id && x.id === s.segment_id);
          if (old && old.hash !== hash) throw fail('reader_pass_segment_conflict');
          outcomes.push({ segment_id: s.segment_id, outcome: old ? 'duplicate' : 'accepted' }); if (!old) fresh.push([s, hash]);
        }
        if (fresh.length) {
          if (p.status !== 'in_progress') throw fail('reader_pass_closed');
          if (p.revision_id !== it.revision_id) throw fail('stale_item_revision');
          const ss = sessions.get(session_id);
          if (flags.expireNextSession) { flags.expireNextSession = false; sessions.delete(session_id); throw fail('reader_pass_session_expired'); }
          const elapsed = ss ? clk() - ss.mono : -1;
          if (!ss || ss.pass_id !== pass_id || elapsed > 360000) { sessions.delete(session_id); throw fail('reader_pass_session_expired'); }
          for (const [s] of fresh) if (!(s.start_offset_ms >= 0 && s.start_offset_ms < s.end_offset_ms && s.end_offset_ms - s.start_offset_ms <= 300000 && s.end_offset_ms <= 300000 && s.end_offset_ms <= elapsed + 1)) throw fail('invalid_reader_pass_timing');
          for (const [s, hash] of fresh) { t.seq += 1; t.segs.push({ sid: session_id, id: s.segment_id, hash, mode: s.mode, a: ss.utc + s.start_offset_ms, b: ss.utc + s.end_offset_ms, seq: t.seq }); }
          t.version = hexv('b');
        }
        const result = { item_id, pass_id, session_id, outcomes, totals: totals(pass_id) };
        if (flags.lostNextReport) { flags.lostNextReport = false; throw fail('unknown'); }
        return result;
      },
    };
    return api;
  }
  // Method log for the DOM tests (reader reads, explicit translation requests).
  const calls = [];
  let generation = 1;
  const views = new Map();
  let viewSeq = 0;
  const jobs = new Map(); const jobsById = new Map(); let jobSeq = 0;

  const guard = async () => { await delay(120); if (flags.offline) throw fail('unavailable'); };
  const counts = (list, key) => [...list.reduce((m, it) => m.set(it[key], (m.get(it[key]) || 0) + 1), new Map())]
    .sort((a, b) => b[1] - a[1]).map(([value, count]) => ({ value, count }));
  // demo-1 is long (> 16KiB) so the segmented reader path is exercised. The synthetic translation
  // keeps the original's blocks in the same order (as the agent's translation does), so 对照 pairs.
  const doc = (it, lang) => {
    const paras = bodyOf(it, lang);
    const heading = (i) => (lang === 'en' ? `## Section ${i + 1}` : `## 第 ${i + 1} 节`);
    const n = { 'demo-1': 24, 'demo-6': 8 }[it.id];
    return (n ? Array.from({ length: n }, (_, i) => [heading(i), ...paras]).flat() : paras).join('\n\n');
  };
  const LONG_ZH = longChineseTranscript();
  const talkEnZh = () => {
    const bs = TALK_EN_ZH.split('\n\n');
    if (flags.talkEnVariant === 'merged') bs.splice(2, 2, `${bs[2]}${bs[3]}`);
    if (flags.talkEnVariant === 'reordered') for (let i = 0; i < bs.length; i += 8) [bs[i + 4], bs[i + 5]] = [bs[i + 5], bs[i + 4]];
    return bs.join('\n\n');
  };
  // reader_v1 editions of the paired book: each side's chapters joined into one body whose
  // chapter ranges partition it exactly (UTF-8 bytes). Rebuilt when a chapter repair changes text.
  const textRepaired = new Set(); const alignRepaired = new Set(); const alignGen = new Map();
  let BOOK; let BOOK_CANONICAL; let BLOCKS; const detached = new Set();
  const blocksOf = (side, c) => (textRepaired.has(c[0]) && DEMO_BOOK_REPAIRED[side]?.[c[0]]) || c[3];
  // 编辑 Markdown (synthetic): a saved chapter's exact text by side and key, which replaces its blocks.
  // An edited chapter's paragraph index reads as missing (alignStale holds the original key).
  const edited = { original: new Map(), translation: new Map() }; const alignStale = new Set();
  function buildBook() {
    BLOCKS = { original: new Map(), translation: new Map() };
    BOOK = Object.fromEntries(['original', 'translation'].map((side) => {
      const all = DEMO_BOOK[side].chapters;
      const parts = all.map((c, i) => edited[side].get(c[0]) ?? `${blocksOf(side, c).join('\n\n')}${i < all.length - 1 ? '\n\n' : ''}`);
      const body = enc.encode(parts.join(''));
      let at = 0;
      const chapters = all.map((c, i) => {
        const [key, title, level, , cp] = c;
        // aiSetup({detach}) turns paired chapters into original-only ones (both sides lose the pair).
        const counterpart = detached.has(key) || detached.has(cp) ? null : cp;
        const start = at; at += enc.encode(parts[i]).length;
        // Block i starts after blocks 0..i-1 and their blank-line separators; the last block runs
        // to the chapter end (including a trailing separator), so block ranges partition the chapter.
        let b = 0; const starts = blocksOf(side, c).map((x) => { const s0 = b; b += enc.encode(`${x}\n\n`).length; return s0; });
        BLOCKS[side].set(key, { starts, bytes: at - start });
        return { key, title, level, start, end: at, counterpart };
      });
      return [side, { side, language: DEMO_BOOK[side].language, sha256: `demo-${side}-${body.length}`, bytes: body.length, chapters, body }];
    }));
    BOOK_CANONICAL = dec.decode(BOOK.translation.body);
  }
  buildBook();
  // Paragraph index of an original chapter (null: none stored), as byte ranges.
  const alignRows = (key) => (alignStale.has(key) ? null : (alignRepaired.has(key) ? DEMO_ALIGN[key]?.repaired : DEMO_ALIGN[key]?.rows) ?? null);
  // Retained book revisions (synthetic): every chapter's exact text per side, kept when the book's
  // revision moves, so revision_chunk can read an older version. The current one is read live.
  const bookHistory = new Map();
  const chapterTexts = () => Object.fromEntries(['original', 'translation'].map((s) => [s, new Map(BOOK[s].chapters.map((c) => [c.key, dec.decode(BOOK[s].body.subarray(c.start, c.end))]))]));
  const keepRevision = (it) => { if (it?.book && !bookHistory.has(it.revision_id)) bookHistory.set(it.revision_id, chapterTexts()); };
  // Staged chapter uploads by edit_id, and request_id → edit_id.
  const edits = new Map(); const editByRequest = new Map();
  const hex32 = (str) => { let x = 0x811c9dc5; for (const c of str) x = Math.imul(x ^ c.codePointAt(0), 0x01000193) >>> 0; return x.toString(16).padStart(8, '0'); };
  const alignVersion = (key) => (alignRows(key) ? Array.from({ length: 8 }, (_, i) => hex32(`${key}:${alignGen.get(key) || 0}:${i}`)).join('') : 'missing');
  const sha256hex = async (bytes) => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), (x) => x.toString(16).padStart(2, '0')).join('');
  const chapterBytes = (side, key) => { const c = BOOK[side].chapters.find((x) => x.key === key); return BOOK[side].body.subarray(c.start, c.end); };
  const ALIGN_PAGE = 3; // small on purpose: next_offset / expected_version paging is exercised
  const alignJobs = new Map(); const alignJobsById = new Map(); let alignSeq = 0;
  const JOB_BUDGET = { max_requests: 2, max_input_bytes_per_request: 24576, max_completion_tokens_per_request: 2048 };
  const exactKeys = (o, keys) => o && typeof o === 'object' && Object.keys(o).length === keys.length && keys.every((k) => k in o);
  // 格式整理 (synthetic): a formatted body replaces the generated one; older bodies are kept by revision.
  const bodyEdited = new Map(); const bodyHistory = new Map();
  const original = (it) => (it.book ? BOOK_CANONICAL : bodyEdited.has(it.id) ? bodyEdited.get(it.id) : it.longZh ? LONG_ZH : it.guji ? GUJI_SAMPLE : it.talk ? TALK_SAMPLE : it.talkEn ? TALK_EN : it.markdown ? MARKDOWN_SAMPLE : doc(it, it.english ? 'en' : 'zh'));
  // 重点标注 v2: key terms with exact spans on the original and (translated items) the translation.
  const highlightsOf = (it) => termHighlights(original(it).split('\n\n'), it.translation === 'done' && !it.longZh && !it.guji && !it.talk && !it.markdown ? doc(it, it.english ? 'zh' : 'en').split('\n\n') : null, it.english);
  const allInsights = (it) => { const items = highlightsOf(it);
    return { ...insightsOf(it), highlights: { state: 'succeeded', reason: null, sides: items.some((x) => x.translation) ? 'original_and_translation' : 'original_only', items } }; };
  // Group tree (groups_v1 shape): per-node direct and subtree counts over the synthetic items.
  const children = (id) => GROUPS.filter((g) => g.parent_id === id);
  const subtree = (id) => [id, ...children(id).flatMap((c) => subtree(c.id))];
  const membersOf = (ids) => items.filter((it) => it.groups?.some((g) => ids.includes(g)));
  const preorder = () => GROUPS.filter((g) => !g.parent_id).flatMap(function walk(g) { return [g, ...children(g.id).flatMap(walk)]; });
  // AI minutes and explicit requests (synthetic): demo-2 has long finished minutes, demo-3 failed ones.
  const minutes = new Map([
    ['demo-2', { state: 'succeeded', text: minutesOf(byId.get('demo-2')) }],
    ['demo-3', { state: 'failed', reason: 'provider_timeout' }],
    ['demo-6', { state: 'succeeded', text: minutesOf(byId.get('demo-6')) }],
  ]);
  // AI insights (synthetic): demo-6 and demo-2 have all three; demo-3 has a failed 金句 and an
  // empty 事实核查; anything else starts not generated and is generated on request.
  const NOT = { state: 'not_requested', reason: null, items: [] };
  const insights = new Map();
  const preset = (id) => {
    const all = allInsights(byId.get(id));
    if (id === 'demo-1' || id === 'demo-md') return { highlights: all.highlights, quotes: NOT, fact_check: NOT };
    // The translation changed after the highlights were made: their coordinates are stale.
    if (id === 'demo-7') return { highlights: { state: 'unavailable', reason: 'insight_translation_changed', items: [] }, quotes: NOT, fact_check: NOT };
    // Highlights made when the translation did not pair block-for-block: original side only.
    if (id === 'demo-9') return { highlights: { ...all.highlights, sides: 'original_only', items: all.highlights.items.filter((x) => x.original).map((x) => ({ ...x, translation: null })) }, quotes: NOT, fact_check: NOT };
    if (id === 'demo-3') return { highlights: NOT, quotes: { state: 'failed', reason: 'provider_timeout', items: [] }, fact_check: { state: 'succeeded', reason: null, basis: 'model_knowledge', items: [] } };
    return ['demo-2', 'demo-6', 'demo-talk'].includes(id) ? all : { highlights: NOT, quotes: NOT, fact_check: NOT };
  };
  const requested = new Map(); // item id → ms when 翻译此文 was pressed
  const BATCHES = 6;
  // Server side knows the bytes, so it can cut on a character boundary.
  const firstSegment = (bytes) => {
    let end = Math.min(bytes.length, MAX_SEGMENT);
    while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end -= 1;
    return { text: dec.decode(bytes.subarray(0, end)), end_byte: end };
  };

  // Stored conversations (synthetic, this page session only), like the Mac's item_chat_turns:
  // a successful turn appends the question and the answer; a replayed request id appends nothing.
  let turnSeq = 0;
  const turnsOf = new Map();
  const appendTurn = (item_id, request_id, question, reply, at, origin = null) => {
    const list = turnsOf.get(item_id) || [];
    if (list.some((t) => t.request_id === request_id)) return;
    list.push({ seq: ++turnSeq, role: 'user', content: question, request_id, created_at_ms: at - 60_000, model: null, origin },
      { seq: ++turnSeq, role: 'assistant', content: reply, request_id, created_at_ms: at, model: null, origin });
    turnsOf.set(item_id, list);
  };
  const DAY = 86_400_000;
  // Like the Mac's conversation_import: an older exchange restored from a screenshot, labelled.
  appendTurn('demo-talk', 'mchat_demo_talk_1', '（示例提问）他们说的「可追溯性」具体指什么？',
    '（示例回答）指每个压缩后的句子都能点回原文的位置，读者可以自己核对。', Date.now() - DAY);
  appendTurn('demo-6', 'restored:demo', '（恢复的示例提问）这篇综述适合谁读？',
    '（恢复的示例回答）适合要在个人资料库里做长文档问答的人。', Date.now() - 3 * DAY, 'restored_from_user_screenshot');
  appendTurn('demo-6', 'demo_hist_1', '这篇综述把上下文压缩方法分成了哪几类？',
    '（合成回答）分成**三类**：\n\n- **基于抽取**：保留原文片段，最容易追溯\n- **基于生成**：压缩率最高，但可能*改写失真*\n- **基于结构化建模**：多跳问答上最稳定\n\n详见原文第 `2` 节。', Date.now() - 2 * DAY);
  appendTurn('demo-6', 'demo_hist_2', '作者最后给出的实践建议是什么？',
    '（合成回答）优先保证引用能回到原文的具体位置：在个人资料库里，可追溯性比压缩率更重要。', Date.now() - 2 * DAY + 300_000);

  // 翻译本章 state (synthetic): quotes, jobs by request id / job id, completed projections by chapter.
  const xlQuotes = new Map(); const xlJobs = new Map(); const xlJobsById = new Map(); const xlDone = new Map(); let xlSeq = 0;
  // A completed AI translation of an original-only chapter, published directly (test setup).
  async function aiPublish(chapter, md) {
    const it = byId.get('demo-book');
    const bytes = enc.encode(md); xlSeq += 1;
    const meta = { version: await sha256hex(enc.encode(`demo-xl:${chapter}:${xlSeq}:${md}`)), item_id: it.id, revision_id: it.revision_id, original_chapter: chapter,
      source_sha256: await sha256hex(chapterBytes('original', chapter)), target_language: 'zh-CN', sha256: await sha256hex(bytes), bytes: bytes.length, provenance: 'machine_translation', route: { ...XL_ROUTE } };
    xlDone.set(chapter, { meta, bytes });
    return structuredClone(meta);
  }

  // 译文标记 ledgers (reader_annotations_ai_translation_v1, synthetic), separate from the edition ledgers.
  // Same CAS and single last-exact receipt; a mark is current only while its revision and EVERY segment's
  // version + hash are the chapter's current AI translation. `order` is the mutation sequence.
  const aiLedgers = new Map(); let aiSeq = 0; const aiAnnLog = [];
  const aiLedger = (id) => { if (!aiLedgers.has(id)) aiLedgers.set(id, { version: `aiv_${(aiSeq += 1).toString(36).padStart(6, '0')}`, marks: new Map(), receipt: null }); return aiLedgers.get(id); };
  const aiNext = (L) => { L.version = `aiv_${(aiSeq += 1).toString(36).padStart(6, '0')}`; return aiSeq; };
  const aiSegCurrent = (it, sg) => { const d = xlDone.get(sg.chapter); return !!d && d.meta.revision_id === it.revision_id && d.meta.version === sg.translation_version && d.meta.sha256 === sg.sha256; };
  const aiStatus = (it, m) => (m.anchor.revision_id === it.revision_id && m.anchor.segments.every((sg) => aiSegCurrent(it, sg)) ? 'current' : 'stale');
  const aiView = async (it, L, filter) => sha256hex(enc.encode(JSON.stringify([L.version, it.revision_id, filter ?? null,
    [...xlDone].map(([k, d]) => [k, d.meta.version, d.meta.sha256])])));
  async function aiAnnList(params) {
    await guard();
    const { item_id, offset, limit, expected_version, annotation_space, projection = null, expected_view_version = null } = params;
    calls.push(`aiAnnList:${offset}`);
    if (flags.noAiAnnotations) throw fail('invalid_request');
    if (annotation_space !== 'ai_translation' || !Object.keys(params).every((k) => ['item_id', 'offset', 'limit', 'expected_version', 'annotation_space', 'projection', 'expected_view_version'].includes(k))
      || !(Number.isInteger(limit) && limit >= 1 && limit <= 100) || !Number.isInteger(offset) || offset < 0
      || (expected_version == null) !== (expected_view_version == null) || (expected_version == null && offset !== 0)) throw fail('invalid_reader_annotation');
    const it = byId.get(item_id);
    if (!it) throw fail('library_resource_not_found');
    const L = aiLedger(item_id);
    const view = await aiView(it, L, projection);
    if (expected_version != null && (expected_version !== L.version || expected_view_version !== view)) throw fail('stale_reader_annotations');
    const same = (sg) => !projection || (sg.chapter === projection.chapter && sg.translation_version === projection.translation_version && sg.sha256 === projection.sha256);
    const all = [...L.marks.values()].filter((m) => m.anchor.segments.some(same)).sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const n = Math.min(limit, flags.aiAnnPageSize || 100);
    const page = all.slice(offset, offset + n).map((m) => ({ id: m.id, style: m.style, anchor: structuredClone(m.anchor), status: aiStatus(it, m), order: m.order,
      projections: m.anchor.segments.map((sg) => ({ chapter: sg.chapter, translation_version: sg.translation_version, sha256: sg.sha256, status: m.anchor.revision_id === it.revision_id && aiSegCurrent(it, sg) ? 'current' : 'stale' })) }));
    return { item_id, annotation_space, version: L.version, view_version: view, annotations: page, total: all.length, next_offset: offset + n < all.length ? offset + n : null };
  }
  async function aiAnnEdit(params) {
    await guard();
    aiAnnLog.push(structuredClone(params));
    const { item_id, expected_version, change, annotation_space } = params;
    calls.push(`aiAnnEdit:${change?.op}`);
    if (flags.noAiAnnotations) throw fail('invalid_request');
    if (annotation_space !== 'ai_translation' || Object.keys(params).length !== 4) throw fail('invalid_reader_annotation');
    const it = byId.get(item_id);
    if (!it) throw fail('library_resource_not_found');
    if (flags.readOnly) throw fail('permission_denied');
    if (flags.droppedNextAiAnnEdit) { flags.droppedNextAiAnnEdit = false; throw fail('unknown'); }
    const L = aiLedger(item_id);
    const key = JSON.stringify([expected_version, change]);
    const lose = () => { if (flags.lostNextAiAnnEdit) { flags.lostNextAiAnnEdit = false; throw fail('unknown'); } };
    if (L.receipt?.key === key) { lose(); return structuredClone(L.receipt.result); }
    if (expected_version !== L.version) throw fail('stale_reader_annotations');
    let outcome;
    if (change?.op === 'put') {
      const a = change.annotation; const an = a?.anchor;
      if (!a || typeof a.id !== 'string' || !a.id || !ANN_STYLES.includes(a.style) || !an || an.side !== 'ai_translation' || !Array.isArray(an.segments)
        || !an.segments.length || an.segments.length > 64 || Object.keys(an).length !== 3) throw fail('invalid_reader_annotation');
      const old = L.marks.get(a.id);
      if (old) {
        if (JSON.stringify(old.anchor) !== JSON.stringify(an)) throw fail('reader_annotation_conflict');
        if (aiStatus(it, old) === 'stale') throw fail('stale_reader_translation');
        if (old.style === a.style) throw fail('reader_annotation_conflict');
        old.style = a.style; outcome = 'style_changed';
      } else {
        if (an.revision_id !== it.revision_id) throw fail('stale_item_revision');
        let last = null;
        for (const sg of an.segments) {
          const idx = BOOK.original.chapters.findIndex((c) => c.key === sg.chapter); const oc = BOOK.original.chapters[idx];
          if (!oc || oc.counterpart || !Number.isSafeInteger(sg.start) || !Number.isSafeInteger(sg.end) || !(sg.start < sg.end)) throw fail('invalid_reader_anchor');
          const d = xlDone.get(sg.chapter);
          if (!d || d.meta.version !== sg.translation_version || d.meta.sha256 !== sg.sha256) throw fail('stale_reader_translation');
          const okB = (b) => b === d.bytes.length || (d.bytes[b] & 0xc0) !== 0x80;
          if (sg.end > d.bytes.length || !okB(sg.start) || !okB(sg.end) || (last && (idx < last[0] || (idx === last[0] && sg.start < last[1])))) throw fail('invalid_reader_anchor');
          last = [idx, sg.end];
        }
        const sameKey = (x) => JSON.stringify(x.segments.map((sg) => [sg.chapter, sg.translation_version, sg.sha256, sg.start, sg.end])) + x.revision_id;
        if ([...L.marks.values()].some((m) => m.style === a.style && sameKey(m.anchor) === sameKey(an))) throw fail('reader_annotation_conflict');
        if (L.marks.size >= 4096) throw fail('reader_annotation_limit_exceeded');
        L.marks.set(a.id, { id: a.id, style: a.style, anchor: structuredClone(an), order: 0 }); outcome = 'created';
      }
      L.marks.get(a.id).order = aiNext(L);
    } else if (change?.op === 'delete') {
      if (!L.marks.delete(change.id)) throw fail('reader_annotation_not_found');
      aiNext(L); outcome = 'deleted';
    } else throw fail('invalid_reader_annotation');
    const result = { item_id, annotation_space, version: L.version, id: change.op === 'put' ? change.annotation.id : change.id, outcome };
    L.receipt = { key, result };
    lose();
    return structuredClone(result);
  }
  const XL_ROUTE = { provider_id: 'demo', model_id: 'synthetic-demo-translator', reasoning_effort: 'low' };
  const XL_BUDGET = { max_requests: 1, max_input_bytes_per_request: 32768, max_completion_tokens_per_request: 8192, max_total_input_bytes: 32768, max_total_completion_tokens: 8192 };
  const xlChapter = (item_id, revision_id, chapter) => {
    const it = byId.get(item_id);
    if (!it) throw fail('library_resource_not_found');
    if (revision_id !== it.revision_id) throw fail('stale_item_revision');
    const oc = it.book && BOOK.original.chapters.find((c) => c.key === chapter);
    if (!oc) throw fail('reader_chapter_not_found');
    return oc;
  };

  // 格式整理 state (synthetic): previews, quotes, batches by request / id, restore receipts.
  const fmtPreviews = new Map(); const fmtQuotes = new Map(); const fmtBatches = new Map(); const fmtById = new Map(); const fmtRestores = new Map();
  const formattedAt = new Map(); let fmtSeq = 0;
  const FMT_ROUTE = { provider_id: 'demo', model_id: 'synthetic-demo-formatter', reasoning_effort: 'low' };
  const fmtText = (it, t) => (t.kind === 'body' ? original(it) : dec.decode(chapterBytes(t.side, t.chapter)));
  // The synthetic formatter (no model): every paragraph over 60 characters is split once at the
  // sentence end nearest its middle — a paragraph_boundary edit that keeps every other byte.
  function fmtApply(text) {
    const changes = []; let out = ''; let at = 0; let id = 0;
    for (const m of text.matchAll(/[^\n]+/g)) {
      const para = m[0];
      if ([...para].length <= 60 || /^(#|```|\||>|- |\* |\d+\. )/.test(para)) continue;
      const mid = para.length / 2; let best = -1;
      for (const s of para.matchAll(/[。！？]|\. (?=[A-Z])/g)) { const end = s.index + s[0].length; if (end < para.length - 10 && (best < 0 || Math.abs(end - mid) < Math.abs(best - mid))) best = end; }
      if (best < 0) continue;
      const cut = m.index + best; const sp = text[cut] === ' ' ? 1 : 0;
      const before = text.slice(cut, cut + sp); const after = '\n\n';
      const oldStart = enc.encode(text.slice(0, cut)).length; const newStart = enc.encode(out + text.slice(at, cut)).length;
      out += text.slice(at, cut) + after; at = cut + sp;
      changes.push({ id: `fc_${++id}`, kind: 'paragraph_boundary', basis: 'context_inferred', reason_code: 'paragraph_layout',
        old_range: [oldStart, oldStart + enc.encode(before).length], new_range: [newStart, newStart + 2], old_sha256: null, new_sha256: null, before, after });
    }
    return { text: out + text.slice(at), changes };
  }
  function fmtWrite(it, t, text) {
    if (t.kind === 'body') { bodyHistory.set(it.revision_id, original(it)); bodyEdited.set(it.id, text); bump(it); return; }
    keepRevision(it); edited[t.side].set(t.chapter, text);
    const ok = t.side === 'original' ? t.chapter : BOOK.original.chapters.find((c) => c.counterpart === t.chapter)?.key;
    if (ok) alignStale.add(ok);
    buildBook(); bump(it);
  }
  function fmtSelect(sel) {
    const one = (it, target) => {
      const key = `${it.id}|${target.kind === 'body' ? 'body' : `${target.side}:${target.chapter}`}`;
      const bytes = enc.encode(fmtText(it, target)).length;
      const already = formattedAt.get(key) === it.revision_id;
      return { target_id: `ft_${hex32(`${key}|${it.revision_id}`)}`, item_id: it.id, revision_id: it.revision_id, target, source_sha256: null, source_bytes: bytes,
        state: already ? 'skipped' : 'eligible', reason: already ? 'already_formatted' : null, budget: already ? null : { ...XL_BUDGET, max_requests: 1 }, key };
    };
    if (sel.kind === 'item') {
      const it = byId.get(sel.item_id);
      if (!it) throw fail('library_resource_not_found');
      if (sel.revision_id !== it.revision_id) throw fail('stale_item_revision');
      const t = sel.target;
      if (it.book ? t?.kind !== 'chapter' || !BOOK[t.side]?.chapters.some((c) => c.key === t.chapter) : t?.kind !== 'body') throw fail(it.book ? 'reader_chapter_not_found' : 'invalid_reader_request');
      return [one(it, t.kind === 'body' ? { kind: 'body' } : { kind: 'chapter', side: t.side, chapter: t.chapter })];
    }
    if (!GROUPS.some((g) => g.id === sel.group_id)) throw fail('library_resource_not_found');
    return membersOf(sel.include_descendants ? subtree(sel.group_id) : [sel.group_id]).map((it) => (it.book
      ? { target_id: `ft_${hex32(`${it.id}|book`)}`, item_id: it.id, revision_id: it.revision_id, target: { kind: 'body' }, source_sha256: null, source_bytes: null, state: 'skipped', reason: 'chapter_required', budget: null }
      : one(it, { kind: 'body' })));
  }
  const fmtPreviewView = (pv) => (Date.now() < pv.ready_at ? { preview_id: pv.preview_id, state: 'preparing', quote: null, error_code: null }
    : { preview_id: pv.preview_id, state: 'ready', quote: JSON.parse(JSON.stringify(pv.quote)), error_code: null });
  const fmtDenyCheck = (b) => { if (b.targets.some((t) => flags.denyBatch.includes(t.item_id))) throw fail('library_resource_not_found'); };
  // Targets run in order, one every 600 ms after the request (synthetic; no provider is called).
  async function fmtSettle(b) {
    const due = Math.floor((Date.now() - b.started) / 600);
    let n = 0;
    for (const t of b.targets) {
      if (t.row !== 'queued') { if (t.row !== 'skipped') n += 1; continue; }
      if (n >= due) break;
      n += 1;
      const it = byId.get(t.item_id);
      if (it.revision_id !== t.revision_id) { Object.assign(t, { row: 'skipped', reason: 'revision_changed' }); continue; }
      if (b.outcome === 'failed') { Object.assign(t, { row: 'failed', error: 'content_format_unavailable' }); continue; }
      if (b.outcome === 'unknown') { Object.assign(t, { row: 'outcome_unknown', error: 'provider_outcome_unknown' }); continue; }
      const before = fmtText(it, t.target); const r = fmtApply(before);
      const input_revision_id = it.revision_id;
      if (r.changes.length) fmtWrite(it, t.target, r.text);
      formattedAt.set(t.key, it.revision_id);
      t.proof = { version: Array.from({ length: 8 }, (_, i) => hex32(`${t.target_id}:${b.batch_id}:${i}`)).join(''), input_revision_id, result_revision_id: it.revision_id,
        source_sha256: await sha256hex(enc.encode(before)), result_sha256: await sha256hex(enc.encode(r.text)), changes: r.changes, before };
      t.row = 'succeeded';
    }
    b.done = b.targets.every((t) => t.row !== 'queued');
  }
  const fmtUsage = (b) => { const ran = b.targets.filter((t) => ['succeeded', 'failed', 'outcome_unknown'].includes(t.row)).length;
    return { requests_dispatched: ran, reported_total_tokens: b.outcome === 'unknown' ? null : ran * 900, unknown_usage_requests: b.outcome === 'unknown' ? ran : 0,
      input_tokens: b.outcome === 'unknown' ? null : ran * 700, output_tokens: b.outcome === 'unknown' ? null : ran * 200 }; };
  function fmtBatchView(b) {
    const counts = { total: b.targets.length, queued: 0, running: 0, succeeded: 0, failed: 0, outcome_unknown: 0, skipped: 0 };
    for (const t of b.targets) counts[t.row] += 1;
    return { batch_id: b.batch_id, request_id: b.request_id, state: b.done ? 'done' : counts.queued === counts.total - counts.skipped ? 'queued' : 'running', counts, usage: fmtUsage(b), error_code: null };
  }
  function fmtRowView(b, t) {
    const ran = ['succeeded', 'failed', 'outcome_unknown'].includes(t.row);
    const job = t.row === 'skipped' || t.row === 'queued' ? null : { job_id: `fj_${t.target_id}`, request_id: b.request_id, operation: 'content_format', item_id: t.item_id, revision_id: t.revision_id,
      target: { ...t.target }, state: t.row, stage: ran ? 'done' : 'formatting', result_revision_id: t.proof?.result_revision_id ?? null, error_code: t.error ?? null,
      failure_stage: t.row === 'failed' ? 'formatting' : null, failure_reason: null, budget: t.budget, usage: { requests_dispatched: ran ? 1 : 0, reported_total_tokens: b.outcome === 'unknown' ? null : 900,
        unknown_usage_requests: b.outcome === 'unknown' ? 1 : 0, input_tokens: b.outcome === 'unknown' ? null : 700, output_tokens: b.outcome === 'unknown' ? null : 200 }, elapsed_ms: 600 };
    const formatting = t.proof ? { status: 'complete', changed: t.proof.changes.length > 0, applied_count: t.proof.changes.length, correction_count: 0, deleted_blocks: 0, unresolved_count: 0,
      checked_bytes: enc.encode(t.proof.before).length, total_bytes: enc.encode(t.proof.before).length, changes_version: t.proof.version, quality: 'not_certified' } : null;
    return { target_id: t.target_id, item_id: t.item_id, revision_id: t.revision_id, target: { ...t.target }, state: t.row, reason: t.reason ?? null, job, formatting };
  }

  // 条目封面 (item_cover_v1): independent of the item text. Original ≤ 480 px wide (several 96 KiB-sized
  // demo chunks), thumbnail PNG with the longest edge 320. A version change draws the next palette.
  const COVER_ART = { 'demo-talk': { w: 480, h: 270, hue: 345 }, 'demo-talk-en': { w: 360, h: 360, hue: 300 }, 'demo-2': { w: 480, h: 270, hue: 160 } };
  const COVER_CHUNK = 96 * 1024;
  const sha = async (bytes) => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), (b) => b.toString(16).padStart(2, '0')).join('');
  const covers = new Map(); // item_id → Promise<{ n, cover, raster, thumbnail }>
  const drawCover = (id, n) => {
    const a = COVER_ART[id]; const hue = a.hue + (n - 1) * 170; const px = coverPixel(hue);
    const s = 320 / Math.max(a.w, a.h); const raster = pngOf(a.w, a.h, px); const thumbnail = pngOf(Math.round(a.w * s), Math.round(a.h * s), px);
    return Promise.all([sha(raster), sha(thumbnail), sha(enc.encode(`${id}:${n}`))]).then(([rs, ts, vs]) => {
      const it = byId.get(id);
      return { n, raster, thumbnail, cover: { item_id: id, version: vs, source_revision_id: it.revision_id,
        source: { episode_url: it.source, youtube_video_id: `demo${String(n).padStart(7, '0')}`, image_url: `https://i.ytimg.com/vi/demo${String(n).padStart(7, '0')}/maxresdefault.jpg`, rss_sha256: vs },
        raster: { sha256: rs, media_type: 'image/png', width: a.w, height: a.h, byte_size: raster.length },
        thumbnail: { sha256: ts, media_type: 'image/png', width: Math.round(a.w * s), height: Math.round(a.h * s), byte_size: thumbnail.length },
        sensitivity: it.sensitivity || 'private' } };
    });
  };
  const coverOf = (id) => { if (!COVER_ART[id] || !byId.has(id)) return null; if (!covers.has(id)) covers.set(id, drawCover(id, 1)); return covers.get(id); };
  const coverGuard = async () => { await delay(60); if (flags.offline) throw fail('unavailable'); if (flags.noCover) throw fail('invalid_request'); };

  return {
    // memex.library.item_cover_batch: one nullable Cover per requested position (≤ 50 ids).
    async itemCoverBatch(params) {
      calls.push(`coverBatch:${(params?.item_ids || []).join(',')}`);
      await coverGuard();
      if (!exactKeys(params, ['item_ids']) || !Array.isArray(params.item_ids) || params.item_ids.length > 50
        || params.item_ids.some((id) => typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,256}$/.test(id))) throw fail('invalid_cover_request');
      return Promise.all(params.item_ids.map(async (id) => (flags.coverNull || !coverOf(id) ? null : structuredClone((await coverOf(id)).cover))));
    },
    // memex.library.item_cover_chunk: current version only, ≤ 96 KiB per piece.
    async itemCoverChunk(params) {
      const { item_id, version, thumbnail, offset } = params || {};
      calls.push(`coverChunk:${item_id}:${thumbnail ? 'thumb' : 'orig'}:${offset}`);
      await coverGuard();
      if (!exactKeys(params, ['item_id', 'version', 'thumbnail', 'offset']) || typeof item_id !== 'string' || !/^[a-f0-9]{64}$/.test(version || '')
        || typeof thumbnail !== 'boolean' || !Number.isSafeInteger(offset) || offset < 0) throw fail('invalid_cover_request');
      const cur = coverOf(item_id) && await coverOf(item_id);
      if (!cur) throw fail('library_resource_not_found');
      if (cur.cover.version !== version) throw fail('stale_cover_version');
      const d = thumbnail ? cur.cover.thumbnail : cur.cover.raster;
      let bytes = thumbnail ? cur.thumbnail : cur.raster;
      if (flags.coverCorrupt === 'total') bytes = bytes.subarray(0, bytes.length - 1);
      if (offset > bytes.length) throw fail('invalid_cover_offset');
      const end = Math.min(bytes.length, offset + COVER_CHUNK);
      const piece = bytes.slice(offset, end);
      if (flags.coverCorrupt === 'hash' && offset === 0) piece[piece.length >> 1] ^= 0x5a;
      // The version moves on (as a new cover published on the Mac) right after this first piece.
      if (offset === 0 && flags.staleCoverReads > 0) { flags.staleCoverReads -= 1; covers.set(item_id, drawCover(item_id, cur.n + 1)); }
      let bin = ''; for (const b of piece) bin += String.fromCharCode(b);
      return { media_type: d.media_type, sha256: d.sha256, total_bytes: bytes.length, offset, next_offset: end < bytes.length ? end : null, data_base64: btoa(bin) };
    },
    coverOf: async (id) => (coverOf(id) ? structuredClone((await coverOf(id)).cover) : null),
    // Synthetic answer only; it is labelled as a sample in the UI and never claims a real model.
    async itemChat({ id, requestId, messages }) {
      await new Promise((r) => setTimeout(r, 600));
      if (flags.offline) throw fail('unavailable');
      const q = messages[messages.length - 1].content;
      const earlier = messages.filter((m) => m.role === 'user').length - 1;
      const reply = `（示例回复）你问的是「${q.slice(0, 40)}」。${earlier ? `我也看到了前面的 ${earlier} 个问题。` : ''}连接你的 Mac 后，这里会由你设置的对话模型就这篇文章回答。`;
      if (id && requestId) appendTurn(id, requestId, q, reply, Date.now());
      return { reply, model: null, finish: 'stop' };
    },
    async conversationRead(item_id) {
      await guard();
      if (!byId.has(item_id)) throw fail('library_resource_not_found');
      const turns = (turnsOf.get(item_id) || []).map((t) => ({ ...t }));
      return { turns, version: `v1:${turns.at(-1)?.seq ?? 0}`, partial: false };
    },
    kind: 'demo',
    flags,
    // A new revision of an item (as an edit on the Mac), for the 文章已更新 path.
    reviseItem(id) { const it = byId.get(id); if (it) bump(it); return it?.revision_id; },
    stateOf: (id) => readerStates.get(id) || null,
    async readerState({ item_id }) {
      await guard(); calls.push('readerState');
      if (!byId.has(item_id)) throw fail('library_resource_not_found');
      const st = readerStates.get(item_id) || EMPTY_STATE();
      return { version: await stateVersion(st), state: structuredClone(st) };
    },
    async readerSave({ item_id, expected_version, state: next }) {
      await guard(); calls.push(`readerSave:${next.visit_id}`);
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (!next.visit_id || next.bookmarks.length > 4096) throw fail('invalid_reader_state');
      const prev = readerStates.get(item_id) || EMPTY_STATE();
      if (flags.foreignNextStateSave) {
        flags.foreignNextStateSave = false;
        readerStates.set(item_id, { visit_id: 'other-device-visit', position: prev.position, bookmarks: [...prev.bookmarks, { id: 'bm-other-device', label: '另一台设备的书签', anchor: { revision_id: it.revision_id, side: 'source', sha256: srcSha(it), chapter: 'section-0', offset: 0 } }] });
        throw fail('stale_reader_state');
      }
      const known = new Set(prev.bookmarks.map((b) => JSON.stringify(b)));
      for (const a of [next.position, ...next.bookmarks.filter((b) => !known.has(JSON.stringify(b))).map((b) => b.anchor)].filter(Boolean)) {
        if (a.revision_id !== it.revision_id) throw fail('stale_item_revision');
        const ch = sourceChapters(it).find((c) => c.key === a.chapter);
        if (a.side !== 'source' || a.sha256 !== srcSha(it) || !ch || a.offset > ch.end - ch.start) throw fail('invalid_reader_anchor');
      }
      if (JSON.stringify(prev) === JSON.stringify(next) && readerStates.has(item_id)) return { version: await stateVersion(prev), state: structuredClone(prev) };
      if (await stateVersion(prev) !== expected_version) throw fail('stale_reader_state');
      if (prev.visit_id !== next.visit_id) latestItem = item_id;
      readerStates.set(item_id, structuredClone(next));
      if (flags.lostNextStateSave) { flags.lostNextStateSave = false; throw fail('unknown'); }
      return { version: await stateVersion(next), state: structuredClone(next) };
    },
    async readerResume() {
      await guard(); calls.push('readerResume');
      return latestItem && byId.has(latestItem) ? [latestItem, structuredClone(readerStates.get(latestItem))] : null;
    },
    // 手工标记 (Manual reader annotations v1, synthetic): memex.library.reader.annotations.list / .edit.
    // A versioned per-item ledger; list pages sorted by id and bound to one version (a later page of
    // another version is stale_reader_annotations); edit is one put/delete under CAS. The last mutation
    // is kept as a receipt, so an exact retry returns its result without a second mutation.
    async readerAnnotationsList(params) {
      if (params && Object.hasOwn(params, 'annotation_space')) return aiAnnList(params);
      const { item_id, offset, limit, expected_version } = params || {};
      await guard(); calls.push(`annList:${offset}`);
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (!(Number.isInteger(limit) && limit >= 1 && limit <= 100) || !Number.isInteger(offset) || offset < 0) throw fail('invalid_request');
      const L = ledger(item_id);
      if (expected_version != null && expected_version !== L.version) throw fail('stale_reader_annotations');
      if (expected_version == null && offset !== 0) throw fail('invalid_request');
      const all = [...L.marks.values()].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      const n = Math.min(limit, flags.annPageSize || 100);
      const page = all.slice(offset, offset + n).map((m) => ({ ...structuredClone(m), status: m.anchor.revision_id === it.revision_id && m.anchor.sha256 === srcSha(it) ? 'current' : 'stale' }));
      return { item_id, version: L.version, annotations: page, total: all.length, next_offset: offset + n < all.length ? offset + n : null };
    },
    async readerAnnotationsEdit(params) {
      if (params && Object.hasOwn(params, 'annotation_space')) return aiAnnEdit(params);
      const { item_id, expected_version, change } = params || {};
      await guard(); calls.push(`annEdit:${change?.op}`);
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      const L = ledger(item_id);
      const key = JSON.stringify([expected_version, change]);
      if (L.receipt?.key === key) { if (flags.lostNextAnnEdit) { flags.lostNextAnnEdit = false; throw fail('unknown'); } return structuredClone(L.receipt.result); }
      if (flags.foreignNextAnnEdit) {
        flags.foreignNextAnnEdit = false;
        L.marks.set('am_other_device', { id: 'am_other_device', style: 'yellow', anchor: { revision_id: it.revision_id, side: 'source', sha256: srcSha(it), segments: [{ chapter: sourceChapters(it)[0].key, start: 0, end: 3 }] } });
        L.version = nextAnnVersion(); L.receipt = null;
      }
      if (expected_version !== L.version) throw fail('stale_reader_annotations');
      let outcome;
      if (change?.op === 'put') {
        const a = change.annotation;
        if (!a || typeof a.id !== 'string' || !a.id || !ANN_STYLES.includes(a.style) || !a.anchor) throw fail('invalid_request');
        if (a.anchor.side === 'ai_translation' || 'translation_version' in (a.anchor.segments?.[0] || {})) throw fail('invalid_reader_annotation'); // an AI anchor needs the space
        const old = L.marks.get(a.id);
        if (old) {
          if (JSON.stringify(old.anchor) !== JSON.stringify(a.anchor)) throw fail('reader_annotation_anchor_changed');
          if (old.style === a.style) throw fail('reader_annotation_conflict');
          old.style = a.style; outcome = 'restyled';
        } else {
          const an = a.anchor; const chs = sourceChapters(it);
          if (an.side !== 'source' || an.revision_id !== it.revision_id || an.sha256 !== srcSha(it)) throw fail('stale_item_revision');
          if (!Array.isArray(an.segments) || !an.segments.length || an.segments.length > 64) throw fail('invalid_reader_annotation');
          const body = enc.encode(original(it)); let last = -1;
          for (const sg of an.segments) {
            const ch = chs.find((c) => c.key === sg.chapter);
            const okB = (b) => b >= body.length || (body[b] & 0xc0) !== 0x80;
            if (!ch || !(sg.start < sg.end) || sg.end > ch.end - ch.start || !okB(ch.start + sg.start) || !okB(ch.start + sg.end) || ch.start + sg.start < last) throw fail('invalid_reader_anchor');
            last = ch.start + sg.end;
          }
          if ([...L.marks.values()].some((m) => m.style === a.style && JSON.stringify(m.anchor) === JSON.stringify(an))) throw fail('reader_annotation_conflict');
          if (L.marks.size >= 4096) throw fail('reader_annotations_full');
          L.marks.set(a.id, { id: a.id, style: a.style, anchor: structuredClone(an) }); outcome = 'created';
        }
      } else if (change?.op === 'delete') {
        if (!L.marks.delete(change.id)) throw fail('reader_annotation_not_found');
        outcome = 'deleted';
      } else throw fail('invalid_request');
      L.version = nextAnnVersion();
      const result = { item_id, version: L.version, id: change.op === 'put' ? change.annotation.id : change.id, outcome };
      L.receipt = { key, result };
      if (flags.lostNextAnnEdit) { flags.lostNextAnnEdit = false; throw fail('unknown'); }
      if (flags.failAfterNextAnnCommit) { flags.failAfterNextAnnCommit = false; throw fail('internal_error'); }
      return structuredClone(result);
    },
    annotationsOf: (id) => [...ledger(id).marks.values()].map((m) => structuredClone(m)),
    aiAnnotationsOf: (id) => [...aiLedger(id).marks.values()].map((m) => structuredClone(m)),
    aiAnnLog,
    // Test setup (synthetic): detach paired chapters, then publish (or replace) completed AI translations
    // of original-only chapters — a replaced one gets a new version and hash, so its marks go stale.
    async aiSetup({ detach = [], translations = {} } = {}) {
      for (const k of detach) detached.add(k);
      if (detach.length) buildBook();
      for (const [chapter, md] of Object.entries(translations)) await aiPublish(chapter, md);
    },
    aiPublish,
    ...passesApi(),
    calls,

    // memex.library.reader.toc: ReaderDocument. Ordinary items have one 'source' edition.
    async readerToc({ item_id, revision_id }) {
      await guard(); calls.push('readerToc');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (revision_id !== it.revision_id) throw fail('stale_item_revision');
      if (it.book && flags.staleBook) throw fail('stale_book_manifest');
      const bytes = enc.encode(original(it)).length;
      const editions = it.book ? ['original', 'translation'].map((s) => { const { body, ...e } = BOOK[s]; return { ...e, chapters: e.chapters.map((c) => ({ ...c })) }; })
        : [{ side: 'source', language: it.english ? 'en' : 'zh', sha256: srcSha(it), bytes, chapters: sourceChapters(it) }];
      return { item_id, revision_id, canonical_sha256: `demo-${bytes}`, default_side: it.book ? 'translation' : 'source',
        alignment: it.book ? 'declared_chapter_order_not_paragraph_alignment' : 'none', editions };
    },
    // memex.library.reader.chunk: offset relative to the chapter, never crossing into the next one.
    async readerChunk({ item_id, revision_id, side, chapter, offset, limit }, { background = false } = {}) {
      await guard(); calls.push(`readerChunk:${side}:${chapter}:${offset}`);
      if (background && flags.busyPrefetch) throw fail('library_loading');
      if (!(Number.isInteger(limit) && limit >= 4 && limit <= 32768)) throw fail('invalid_reader_limit');
      if (!Number.isSafeInteger(offset) || offset < 0) throw fail('invalid_reader_offset');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (revision_id !== it.revision_id) throw fail('stale_item_revision');
      if (!it.book) throw fail('reader_side_not_found');
      if (flags.staleBook) throw fail('stale_book_manifest');
      const e = BOOK[side];
      if (!e) throw fail('reader_side_not_found');
      const ch = e.chapters.find((c) => c.key === chapter);
      if (!ch) throw fail('reader_chapter_not_found');
      const start = ch.start + offset;
      const boundary = (n) => n === e.body.length || (e.body[n] & 0xc0) !== 0x80;
      if (start > ch.end || !boundary(start)) throw fail('invalid_reader_offset');
      let end = Math.min(start + Math.min(limit, DEMO_CHUNK), ch.end);
      while (!boundary(end)) end -= 1;
      return { item_id, revision_id, side, sha256: e.sha256, evidence_id: `demo_evidence_${side}`, chapter, start, end,
        next_offset: end < ch.end ? end - ch.start : null, markdown: dec.decode(e.body.subarray(start, end)) };
    },

    // Paragraph alignment capability and grant as the demo's session.info reports them.
    alignCaps() {
      return { reader_alignment: flags.noAlignment ? 'unavailable' : 'available', reader_repair: flags.noRepair ? 'unavailable' : 'available',
        reader_translation: flags.noTranslation ? 'unavailable' : 'available',
        reader_edit: flags.noEdit ? 'unavailable' : 'available', reader_revision_text: flags.noRevisionText ? 'unavailable' : 'available',
        reader_translation_metadata: flags.noTranslationMetadata ? 'unavailable' : 'available',
        reader_translation_alignment: flags.noTranslationMetadata || flags.noTranslationAlignment ? 'unavailable' : 'available',
        reader_format: flags.noFormat ? 'unavailable' : 'available', reader_format_batch: flags.noFormat || flags.noFormatBatch ? 'unavailable' : 'available',
        reader_state_sync_v1: flags.noStateSync ? 'unavailable' : 'available', reader_annotations_v1: flags.annotations ? 'available' : 'unavailable',
        reader_passes_v1: flags.noPasses ? 'unavailable' : 'available',
        reader_annotations_ai_translation_v1: flags.noAiAnnotations ? 'unavailable' : 'available',
        item_cover_v1: flags.noCover ? 'unavailable' : 'available',
        ui_active_group_v1: flags.noActiveGroup ? 'unavailable' : 'available',
        scopes: flags.readOnly ? ['read'] : ['read', 'edit', 'capture'] };
    },
    // memex.library.reader.alignment: one page of pair rows of an ORIGINAL chapter (frozen seam).
    async readerAlignment(params) {
      await guard();
      const { item_id, revision_id, chapter, offset, limit, expected_version } = params || {};
      calls.push(`readerAlignment:${chapter}:${offset}:${expected_version === 'any' ? 'any' : expected_version === 'missing' ? 'missing' : 'v'}`);
      if (flags.noAlignment) throw fail('invalid_request');
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter', 'offset', 'limit', 'expected_version'])
        || !Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(limit) || limit < 1 || limit > 256 || typeof expected_version !== 'string') throw fail('invalid_reader_alignment_request');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (revision_id !== it.revision_id) throw fail('stale_item_revision');
      const oc = it.book && BOOK.original.chapters.find((c) => c.key === chapter);
      if (!oc) throw fail('reader_chapter_not_found');
      const version = alignVersion(chapter);
      if (expected_version !== 'any' && expected_version !== version) throw fail('stale_chapter_alignment');
      if (flags.staleAlignOnce && offset > 0) { flags.staleAlignOnce = false; alignGen.set(chapter, (alignGen.get(chapter) || 0) + 1); throw fail('stale_chapter_alignment'); }
      const tk = oc.counterpart;
      const ob = BLOCKS.original.get(chapter); const tb = tk ? BLOCKS.translation.get(tk) : null;
      const range = (bl, r) => (r ? [bl.starts[r[0]], r[1] + 1 < bl.starts.length ? bl.starts[r[1] + 1] : bl.bytes] : null);
      const rows = alignRows(chapter) || [];
      const all = rows.map(([o, t, kind, relation]) => ({ o: range(ob, o), t: tb ? range(tb, t) : null, kind, relation }));
      if (flags.corruptAlignment && all[1]?.o) all[1] = { ...all[1], o: [all[1].o[0] + 1, all[1].o[1]] };
      const matched = rows.some((r) => r[3] === 'matched' && r[2] === 'paragraph');
      const status = !rows.length || !matched ? 'none' : rows.some((r) => r[3] === 'unresolved') ? 'partial' : 'aligned';
      const pairs = all.slice(offset, offset + Math.min(limit, ALIGN_PAGE));
      const next = offset + pairs.length;
      return { schema_version: 1, item_id, revision_id, version, original_chapter: chapter, translation_chapter: tk,
        original_sha256: await sha256hex(chapterBytes('original', chapter)), translation_sha256: tk ? await sha256hex(chapterBytes('translation', tk)) : null,
        original_bytes: ob.bytes, translation_bytes: tb ? tb.bytes : null, status, quality: 'not_certified',
        offset, total: all.length, next_offset: next < all.length ? next : null, pairs };
    },
    // memex.library.reader.align_request: explicit chapter repair job (edit grant). Same request_id
    // and payload replays the same job; nothing here calls a provider or writes a real Library.
    async readerAlignRequest(params) {
      await guard();
      const { item_id, revision_id, chapter, request_id, expected_alignment_version } = params || {};
      calls.push(`alignRequest:${chapter}:${request_id}`);
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter', 'request_id', 'expected_alignment_version'])
        || typeof request_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(request_id) || typeof expected_alignment_version !== 'string') throw fail('invalid_reader_alignment_request');
      if (flags.readOnly) throw fail('permission_denied');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      const same = alignJobs.get(request_id);
      if (same) {
        if (same.item_id !== item_id || same.revision_id !== revision_id || same.chapter !== chapter || same.expected !== expected_alignment_version) throw fail('request_identity_conflict');
        return alignJobView(same);
      }
      if (revision_id !== it.revision_id) throw fail('stale_item_revision');
      const oc = it.book && BOOK.original.chapters.find((c) => c.key === chapter);
      if (!oc || !oc.counterpart) throw fail('reader_chapter_not_found');
      if (alignVersion(chapter) !== expected_alignment_version) throw fail('stale_chapter_alignment');
      if ([...alignJobs.values()].some((j) => j.item_id === item_id && ['queued', 'running'].includes(alignJobView(j).state))) throw fail('reader_job_in_progress');
      const j = { job_id: `job_align_${(alignSeq += 1).toString(16).padStart(8, '0')}`, request_id, item_id, revision_id, chapter, expected: expected_alignment_version,
        started: Date.now(), fail: flags.failNextJob, done: null };
      flags.failNextJob = false;
      alignJobs.set(request_id, j); alignJobsById.set(j.job_id, j);
      if (flags.unknownNextAlignRequest) { flags.unknownNextAlignRequest = false; throw fail('unknown'); }
      return alignJobView(j);
    },
    async readerAlignStatus(params) {
      await guard();
      const { item_id, job_id } = params || {};
      calls.push(`alignStatus:${job_id}`);
      if (!exactKeys(params, ['item_id', 'job_id'])) throw fail('invalid_reader_alignment_request');
      const j = alignJobsById.get(job_id);
      if (!j || j.item_id !== item_id) throw fail('reader_job_not_found');
      return alignJobView(j);
    },

    // 翻译本章 (frozen reader_chapter_translation_v1 shapes). SYNTHETIC: no provider, credential or
    // network is ever touched; the "translation" is fixed demo text and usage reports zero dispatches
    // (a simulated failed / unknown job reports one dispatch of unknown usage).
    async readerTranslatePreview(params) {
      await guard();
      const { item_id, revision_id, chapter } = params || {};
      calls.push(`translatePreview:${chapter}`);
      if (flags.noTranslation) throw fail('invalid_request');
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter'])) throw fail('invalid_reader_translation_request');
      const oc = xlChapter(item_id, revision_id, chapter);
      if (oc.counterpart) return { eligible: false, reason: 'reader_translation_not_needed', quote: null, translation: null };
      const done = xlDone.get(chapter);
      if (done) return { eligible: true, reason: null, quote: null, translation: done.meta };
      const quote_id = `quote_${hex32(`q${(xlSeq += 1)}`)}${hex32(chapter)}${hex32(String(xlSeq))}${hex32('demo')}`;
      const source_bytes = chapterBytes('original', chapter).length;
      const q = { quote_id, expires_at: Date.now() + 15 * 60_000, item_id, revision_id, chapter, source_sha256: await sha256hex(chapterBytes('original', chapter)),
        target_language: 'zh-CN', settings_revision: 1, route: { ...XL_ROUTE }, budget: { ...XL_BUDGET }, source_bytes, planned_requests: 1, estimated_cost: null };
      xlQuotes.set(quote_id, q);
      return { eligible: true, reason: null, quote: q, translation: null };
    },
    async readerTranslateRequest(params) {
      await guard();
      const { item_id, revision_id, chapter, request_id, quote_id } = params || {};
      calls.push(`translateRequest:${chapter}:${request_id}`);
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter', 'request_id', 'quote_id', 'accept_cost', 'expected_translation_version'])
        || params.accept_cost !== true || params.expected_translation_version !== 'missing' || !/^[A-Za-z0-9_-]{1,128}$/.test(request_id || '')) throw fail('invalid_reader_translation_request');
      if (flags.readOnly) throw fail('permission_denied');
      const same = xlJobs.get(request_id);
      if (same) {
        if (same.item_id !== item_id || same.revision_id !== revision_id || same.chapter !== chapter || same.quote_id !== quote_id) throw fail('request_identity_conflict');
        await xlSettle(same);
        return xlJobView(same);
      }
      const oc = xlChapter(item_id, revision_id, chapter);
      if (oc.counterpart) throw fail('reader_translation_not_needed');
      const q = xlQuotes.get(quote_id);
      if (!q || q.chapter !== chapter || q.expires_at < Date.now()) throw fail('reader_translation_quote_stale');
      if ([...xlJobs.values()].some((j) => j.item_id === item_id && ['queued', 'running'].includes(xlJobView(j).state))) throw fail('reader_job_in_progress');
      const j = { job_id: `job_xl_${(xlSeq += 1).toString(16).padStart(8, '0')}`, request_id, item_id, revision_id, chapter, quote_id, started: Date.now(),
        outcome: flags.failNextTranslation ? 'failed' : flags.unknownNextTranslation ? 'unknown' : 'success', done: null };
      flags.failNextTranslation = false; flags.unknownNextTranslation = false;
      xlJobs.set(request_id, j); xlJobsById.set(j.job_id, j);
      if (flags.lostNextTranslateReply) { flags.lostNextTranslateReply = false; throw fail('unknown'); }
      return xlJobView(j);
    },
    async readerTranslateStatus(params) {
      await guard();
      const { item_id, job_id } = params || {};
      calls.push(`translateStatus:${job_id}`);
      if (!exactKeys(params, ['item_id', 'job_id'])) throw fail('invalid_reader_translation_request');
      const j = xlJobsById.get(job_id);
      if (!j || j.item_id !== item_id) throw fail('reader_job_not_found');
      await xlSettle(j);
      return xlJobView(j);
    },
    async readerTranslateChunk(params) {
      await guard();
      const { item_id, revision_id, chapter, expected_version, offset, limit } = params || {};
      calls.push(`translateChunk:${chapter}:${offset}`);
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter', 'expected_version', 'offset', 'limit'])
        || !Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(limit) || limit < 4 || limit > 32768) throw fail('invalid_reader_translation_request');
      xlChapter(item_id, revision_id, chapter);
      const d = xlDone.get(chapter);
      if (!d || d.meta.version !== expected_version || offset > d.bytes.length || (offset < d.bytes.length && (d.bytes[offset] & 0xc0) === 0x80)) throw fail('stale_reader_translation');
      let end = Math.min(d.bytes.length, offset + Math.min(limit, DEMO_CHUNK));
      while (end < d.bytes.length && (d.bytes[end] & 0xc0) === 0x80) end -= 1;
      return { ...d.meta, start: offset, end, next_offset: end < d.bytes.length ? end : null, markdown: dec.decode(d.bytes.subarray(offset, end)) };
    },

    // translate_metadata (frozen shape, synthetic): the completed projection of this chapter or null.
    async readerTranslateMetadata(params) {
      await guard();
      const { item_id, revision_id, chapter } = params || {};
      calls.push(`translateMetadata:${chapter}`);
      if (flags.noTranslationMetadata) throw fail('invalid_request');
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter'])) throw fail('invalid_reader_translation_request');
      xlChapter(item_id, revision_id, chapter);
      const d = xlDone.get(chapter);
      return { item_id, revision_id, chapter, translation: d && d.meta.revision_id === revision_id ? { ...d.meta, route: { ...d.meta.route } } : null };
    },
    // translate_alignment (frozen shape, synthetic): source blocks paired in order with the AI blocks
    // (the last row takes any remainder on both sides), so both bodies are partitioned exactly.
    async readerTranslateAlignment(params) {
      await guard();
      const { item_id, revision_id, chapter, expected_version, offset, limit } = params || {};
      calls.push(`translateAlignment:${chapter}:${offset}`);
      if (flags.noTranslationMetadata || flags.noTranslationAlignment) throw fail('invalid_request');
      if (!exactKeys(params, ['item_id', 'revision_id', 'chapter', 'expected_version', 'offset', 'limit'])
        || !Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(limit) || limit < 1 || limit > 256) throw fail('invalid_reader_translation_request');
      xlChapter(item_id, revision_id, chapter);
      const d = xlDone.get(chapter);
      if (!d) throw fail('reader_translation_unavailable');
      if (d.meta.version !== expected_version) throw fail('stale_reader_translation');
      const ob = chapterBytes('original', chapter);
      const cuts = (bytes) => { let at = 0; return dec.decode(bytes).split(/(?<=\n\n)/).map((p) => { const a = at; at += enc.encode(p).length; return [a, at]; }); };
      const co = cuts(ob); const ct = cuts(d.bytes); const n = Math.min(co.length, ct.length);
      const all = flags.legacyTranslation ? [] : Array.from({ length: n }, (_, i) => ({ o: [co[i][0], i === n - 1 ? ob.length : co[i][1]], t: [ct[i][0], i === n - 1 ? d.bytes.length : ct[i][1]],
        kind: i === 0 && dec.decode(d.bytes).startsWith('#') ? 'heading' : 'paragraph', relation: 'matched' }));
      if (flags.legacyTranslation && offset !== 0) throw fail('invalid_reader_translation_request');
      const page = all.slice(offset, offset + limit);
      return { item_id, revision_id, version: d.meta.version, original_chapter: chapter, translation_chapter: null, original_sha256: await sha256hex(ob), translation_sha256: d.meta.sha256,
        original_bytes: ob.length, translation_bytes: d.bytes.length, status: flags.legacyTranslation ? 'missing' : 'aligned', quality: 'not_certified', basis: 'translation_blocks',
        offset, total: all.length, next_offset: offset + page.length < all.length ? offset + page.length : null, pairs: page };
    },

    // 编辑 Markdown (frozen reader_chapter_edit_v1 shapes, synthetic): begin → chunks → commit. The
    // stage is keyed by request_id; an exact replay resumes it or returns the committed receipt.
    async readerEditBegin(params) {
      await guard(); calls.push(`readerEditBegin:${params?.side}:${params?.chapter}:${params?.request_id}`);
      const keys = ['item_id', 'revision_id', 'side', 'chapter', 'expected_chapter_sha256', 'request_id', 'bytes', 'sha256'];
      const { item_id, revision_id, side, chapter, expected_chapter_sha256, request_id, bytes, sha256 } = params || {};
      if (flags.noEdit) throw fail('invalid_request');
      if (!exactKeys(params, keys) || !['original', 'translation'].includes(side) || !/^[A-Za-z0-9_-]{1,128}$/.test(request_id || '')
        || !/^[a-f0-9]{64}$/.test(expected_chapter_sha256 || '') || !/^[a-f0-9]{64}$/.test(sha256 || '') || !Number.isSafeInteger(bytes) || bytes < 1) throw fail('invalid_reader_edit');
      if (bytes > 16 * 1024 * 1024) throw fail('reader_edit_too_large');
      if (flags.readOnly) throw fail('permission_denied');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      const known = editByRequest.get(request_id);
      if (known) {
        const s = edits.get(known);
        if (keys.some((k) => s.params[k] !== params[k])) throw fail('reader_edit_upload_conflict');
        return { edit_id: s.edit_id, received_bytes: s.received.length, max_chunk_bytes: 8192, state: s.receipt ? 'committed' : 'uploading', receipt: s.receipt ? { ...s.receipt } : null };
      }
      if (flags.staleNextEdit && it.book) { flags.staleNextEdit = false; bump(it); }
      if (revision_id !== it.revision_id) throw fail('stale_item_revision');
      if (!it.book || !BOOK[side].chapters.some((c) => c.key === chapter)) throw fail('reader_chapter_not_found');
      if ((await sha256hex(chapterBytes(side, chapter))) !== expected_chapter_sha256) throw fail('stale_item_revision');
      const edit_id = `edit_${await sha256hex(enc.encode(`demo-edit:${request_id}`))}`;
      edits.set(edit_id, { edit_id, params: { ...params }, received: new Uint8Array(0), receipt: null });
      editByRequest.set(request_id, edit_id);
      return { edit_id, received_bytes: 0, max_chunk_bytes: 8192, state: 'uploading', receipt: null };
    },
    async readerEditChunk(params) {
      await guard();
      const { item_id, edit_id, offset, markdown } = params || {};
      calls.push(`readerEditChunk:${offset}`);
      if (!exactKeys(params, ['item_id', 'edit_id', 'offset', 'markdown']) || typeof markdown !== 'string' || !markdown || markdown.includes('\0') || !Number.isSafeInteger(offset)) throw fail('invalid_reader_edit');
      const piece = enc.encode(markdown);
      if (piece.length > 8192) throw fail('invalid_reader_edit');
      const s = edits.get(edit_id);
      if (!s || s.params.item_id !== item_id) throw fail('reader_edit_not_found');
      const have = s.received;
      if (offset + piece.length <= have.length) {
        // An already acknowledged range: identical replay succeeds, anything else conflicts.
        if (have.subarray(offset, offset + piece.length).some((b, i) => b !== piece[i])) throw fail('reader_edit_upload_conflict');
        return { edit_id, received_bytes: have.length };
      }
      if (offset !== have.length || have.length + piece.length > s.params.bytes) throw fail('reader_edit_upload_conflict');
      const next = new Uint8Array(have.length + piece.length); next.set(have); next.set(piece, have.length); s.received = next;
      if (flags.lostNextEditChunk) { flags.lostNextEditChunk = false; throw fail('unknown'); }
      return { edit_id, received_bytes: next.length };
    },
    async readerEditCommit(params) {
      await guard();
      const { item_id, edit_id } = params || {};
      calls.push('readerEditCommit');
      if (!exactKeys(params, ['item_id', 'edit_id'])) throw fail('invalid_reader_edit');
      const s = edits.get(edit_id);
      if (!s || s.params.item_id !== item_id) throw fail('reader_edit_not_found');
      if (s.receipt) return { ...s.receipt };
      const p = s.params;
      if (s.received.length !== p.bytes || (await sha256hex(s.received)) !== p.sha256) throw fail('invalid_reader_edit');
      const text = dec.decode(s.received);
      if (!text.trim()) throw fail('invalid_reader_edit');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (it.revision_id !== p.revision_id || (await sha256hex(chapterBytes(p.side, p.chapter))) !== p.expected_chapter_sha256) throw fail('stale_item_revision');
      const same = dec.decode(chapterBytes(p.side, p.chapter)) === text;
      if (!same) {
        keepRevision(it);
        edited[p.side].set(p.chapter, text);
        const ok = p.side === 'original' ? p.chapter : BOOK.original.chapters.find((c) => c.counterpart === p.chapter)?.key;
        if (ok) alignStale.add(ok);
        buildBook(); bump(it);
      }
      s.receipt = { item_id, request_id: p.request_id, revision_id: it.revision_id, side: p.side, chapter: p.chapter, chapter_sha256: p.sha256, changed: !same };
      if (flags.lostNextEditCommit) { flags.lostNextEditCommit = false; throw fail('unknown'); }
      return { ...s.receipt };
    },
    // 对比修改 (frozen reader_revision_text_v1 shape): exact text of one chapter target of a retained
    // revision (or the current one), by bounded pieces bound to the target's sha256.
    async readerRevisionChunk(params) {
      await guard();
      const { item_id, revision_id, target, start_byte, limit_bytes, expected_sha256 } = params || {};
      calls.push(`readerRevisionChunk:${revision_id}:${target?.side}:${start_byte}`);
      if (flags.noRevisionText) throw fail('invalid_request');
      if (!exactKeys(params, ['item_id', 'revision_id', 'target', 'start_byte', 'limit_bytes', 'expected_sha256']) || !target
        || !(target.kind === 'body' || (target.kind === 'chapter' && ['original', 'translation'].includes(target.side) && typeof target.chapter === 'string'))) throw fail('invalid_reader_request');
      if (!Number.isSafeInteger(start_byte) || start_byte < 0) throw fail('invalid_reader_offset');
      if (!Number.isInteger(limit_bytes) || limit_bytes < 1 || limit_bytes > 8192) throw fail('invalid_reader_limit');
      const it = byId.get(item_id);
      if (!it || (!it.book && target.kind !== 'body') || (it.book && target.kind === 'body')) throw fail(it ? 'invalid_reader_request' : 'library_resource_not_found');
      const texts = it.book ? (revision_id === it.revision_id ? chapterTexts() : bookHistory.get(revision_id)) : null;
      const text = it.book ? texts?.[target.side]?.get(target.chapter) : revision_id === it.revision_id ? original(it) : bodyHistory.get(revision_id);
      if (text == null) throw fail('revision_text_unavailable');
      const bytes = enc.encode(text); const sha256 = await sha256hex(bytes);
      if (expected_sha256 !== 'any' && expected_sha256 !== sha256) throw fail('stale_item_revision');
      const boundary = (n) => n === bytes.length || (bytes[n] & 0xc0) !== 0x80;
      if (start_byte > bytes.length || !boundary(start_byte)) throw fail('invalid_reader_offset');
      let end = Math.min(start_byte + limit_bytes, bytes.length);
      while (!boundary(end)) end -= 1;
      return { item_id, revision_id, target: { ...target }, sha256, total_bytes: bytes.length, start_byte, end_byte: end,
        next_offset: end < bytes.length ? end : null, text: dec.decode(bytes.subarray(start_byte, end)) };
    },

    // 格式整理 (frozen Unified content formatting v1 shapes, all synthetic, returned as the gateway
    // source returns them: request / status / resume give the batch itself): preview → one quote →
    // a durable batch whose targets run here without any model.
    async readerFormatPreview(params) {
      await guard(); calls.push(`readerFormatPreview:${params?.preview_id ? 'read' : params?.selection?.kind}`);
      if (flags.noFormat) throw fail('invalid_request');
      if (params?.preview_id) {
        const pv = fmtPreviews.get(params.preview_id);
        if (!pv) throw fail('content_format_preview_not_found');
        return fmtPreviewView(pv);
      }
      const sel = params?.selection;
      if (!exactKeys(params, ['selection']) || !sel || !['item', 'group'].includes(sel.kind)) throw fail('invalid_reader_request');
      if (sel.kind === 'group' && flags.noFormatBatch) throw fail('invalid_request');
      const targets = fmtSelect(sel);
      const eligible = targets.filter((t) => t.state === 'eligible');
      const reasons = {}; for (const t of targets) if (t.reason) reasons[t.reason] = (reasons[t.reason] || 0) + 1;
      const quote = { quote_id: `fq_${++fmtSeq}`, expires_at: Date.now() + 15 * 60000, selection: JSON.parse(JSON.stringify(sel)), policy_version: 'content-format-v1',
        route: eligible.length ? { ...FMT_ROUTE } : null,
        counts: { items: new Set(targets.map((t) => t.item_id)).size, targets: targets.length, eligible: eligible.length, skipped: targets.length - eligible.length,
          private_targets: flags.privateFormat ? eligible.length : 0, skipped_by_reason: reasons },
        budget: { max_requests: eligible.length, max_input_bytes: eligible.reduce((n, t) => n + t.source_bytes + 2048, 0), max_completion_tokens: eligible.length * 4096 },
        estimated_cost: null };
      const pv = { preview_id: `fp_${fmtSeq}`, ready_at: Date.now() + (sel.kind === 'group' ? 500 : 0), quote, targets };
      fmtPreviews.set(pv.preview_id, pv); fmtQuotes.set(quote.quote_id, pv);
      return fmtPreviewView(pv);
    },
    async readerFormatTargets({ preview_id, offset, limit }) {
      await guard();
      const pv = fmtPreviews.get(preview_id);
      if (!pv) throw fail('content_format_preview_not_found');
      const page = pv.targets.slice(offset, offset + limit);
      return { preview_id, total: pv.targets.length, offset, next_offset: offset + page.length < pv.targets.length ? offset + page.length : null, targets: page.map((t) => ({ ...t })) };
    },
    async readerFormatRequest(params) {
      await guard();
      const { quote_id, request_id } = params || {};
      calls.push(`readerFormatRequest:${request_id}`);
      if (flags.readOnly) throw fail('permission_denied');
      if (!exactKeys(params, ['quote_id', 'request_id', 'accept_cost', 'accept_model_processing']) || params.accept_cost !== true || params.accept_model_processing !== true) throw fail('invalid_reader_request');
      const old = fmtBatches.get(request_id);
      if (old) { if (old.quote_id !== quote_id) throw fail('request_identity_conflict'); fmtDenyCheck(old); return fmtBatchView(old); }
      const pv = fmtQuotes.get(quote_id);
      if (!pv) throw fail('content_format_quote_stale');
      if (pv.consumed) throw fail('content_format_quote_consumed');
      if (Date.now() > pv.quote.expires_at) throw fail('content_format_quote_stale');
      pv.consumed = true;
      const b = { batch_id: `fb_${++fmtSeq}`, request_id, quote_id, started: Date.now(), done: false,
        targets: pv.targets.map((t) => ({ ...t, row: t.state === 'eligible' ? 'queued' : 'skipped' })),
        outcome: flags.failNextFormat ? 'failed' : flags.unknownNextFormat ? 'unknown' : 'success' };
      flags.failNextFormat = false; flags.unknownNextFormat = false;
      fmtBatches.set(request_id, b); fmtById.set(b.batch_id, b);
      if (flags.lostNextFormatReply) { flags.lostNextFormatReply = false; throw fail('unknown'); }
      return fmtBatchView(b);
    },
    async readerFormatStatus({ batch_id }) {
      await guard(); calls.push('readerFormatStatus');
      const b = fmtById.get(batch_id);
      if (!b) throw fail('content_format_batch_not_found');
      fmtDenyCheck(b); await fmtSettle(b);
      return fmtBatchView(b);
    },
    async readerFormatResults({ batch_id, offset, limit }) {
      await guard(); calls.push(`readerFormatResults:${offset}`);
      const b = fmtById.get(batch_id);
      if (!b) throw fail('content_format_batch_not_found');
      fmtDenyCheck(b); await fmtSettle(b);
      const page = b.targets.slice(offset, offset + limit);
      return { batch_id, total: b.targets.length, offset, next_offset: offset + page.length < b.targets.length ? offset + page.length : null, results: page.map((t) => fmtRowView(b, t)) };
    },
    async readerFormatChanges(params) {
      await guard(); calls.push(`readerFormatChanges:${params?.view}`);
      const b = fmtById.get(params?.batch_id); const t = b?.targets.find((x) => x.target_id === params.target_id);
      if (!t?.proof || flags.denyBatch.includes(t.item_id)) throw fail('library_resource_not_found');
      const pr = t.proof;
      if (params.expected_version !== pr.version) throw fail('content_format_changes_stale');
      if (params.view === 'text') {
        const c = pr.changes.find((x) => x.id === params.change_id);
        if (!c || !['before', 'after'].includes(params.part)) throw fail('invalid_reader_request');
        const bytes = enc.encode(c[params.part]); const start = params.start_byte;
        let end = Math.min(start + params.limit_bytes, bytes.length);
        while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end -= 1;
        return { version: pr.version, change_id: c.id, part: params.part, total_bytes: bytes.length, start_byte: start, end_byte: end, next_offset: end < bytes.length ? end : null, text: dec.decode(bytes.subarray(start, end)) };
      }
      const page = pr.changes.slice(params.offset, params.offset + params.limit);
      return { version: pr.version, item_id: t.item_id, input_revision_id: pr.input_revision_id, result_revision_id: pr.result_revision_id, target: { ...t.target },
        source_sha256: pr.source_sha256, result_sha256: pr.result_sha256, total: pr.changes.length, offset: params.offset,
        next_offset: params.offset + page.length < pr.changes.length ? params.offset + page.length : null,
        changes: page.map(({ before, after, ...c }) => ({ ...c, before_bytes: enc.encode(before).length, after_bytes: enc.encode(after).length, source_evidence_ids: [] })) };
    },
    async readerFormatRestore(params) {
      await guard();
      const { batch_id, target_id, expected_revision_id, request_id } = params || {};
      calls.push(`readerFormatRestore:${request_id}`);
      if (flags.readOnly) throw fail('permission_denied');
      const done = fmtRestores.get(request_id);
      if (done) { if (done.key !== `${batch_id}|${target_id}`) throw fail('request_identity_conflict'); return { ...done.receipt }; }
      const b = fmtById.get(batch_id); const t = b?.targets.find((x) => x.target_id === target_id);
      if (!t?.proof || flags.denyBatch.includes(t.item_id)) throw fail('library_resource_not_found');
      const it = byId.get(t.item_id);
      if (it.revision_id !== expected_revision_id || expected_revision_id !== t.proof.result_revision_id) throw fail('content_format_restore_conflict');
      fmtWrite(it, t.target, t.proof.before);
      const receipt = { item_id: it.id, request_id, revision_id: it.revision_id, changed: true };
      fmtRestores.set(request_id, { key: `${batch_id}|${target_id}`, receipt });
      if (flags.lostNextRestoreReply) { flags.lostNextRestoreReply = false; throw fail('unknown'); }
      return { ...receipt };
    },
    async readerFormatResume({ batch_id }) {
      await guard(); calls.push('readerFormatResume');
      const b = fmtById.get(batch_id);
      if (!b) throw fail('content_format_batch_not_found');
      fmtDenyCheck(b); await fmtSettle(b);
      return fmtBatchView(b);
    },

    // What a Mac's memex.session.info reports, for the demo: everything served, no versions
    // (the 版本 page says it is not connected rather than inventing a Mac build).
    async sessionInfo() {
      await guard();
      return { insights: 'available', conversation: 'available', groups: 'available', reader: 'available', ...this.alignCaps(), versions: null, demo: true,
        query_sorts: ['added_at', 'captured_at', 'published_at', 'title', 'source', 'item_type'], query_origin: 'available' };
    },
    // 当前分组 (synthetic, interfaces/memex.chrome_capture_v1.md): every request is logged in
    // activeGroupLog; activeGroup is the last accepted acknowledgement. An edit-grant write keyed by
    // request_id: an exact replay returns the original acknowledgement without becoming latest again,
    // changed params conflict. The acknowledgement's request_id is namespaced as the gateway does
    // (here by a fixed demo principal). Demo group ids are checked against the fixture tree, not the
    // real `group_…` id form.
    activeGroupLog: [], activeGroup: null, activeGroupReceipts: new Map(),
    async uiActiveGroupSet(params) {
      await guard();
      this.activeGroupLog.push(structuredClone(params));
      calls.push(`activeGroupSet:${params?.group_id ?? 'null'}`);
      if (flags.noActiveGroup) throw fail('invalid_request');
      if (flags.readOnly) throw fail('permission_denied');
      const { request_id, group_id } = params || {};
      if (!exactKeys(params, ['request_id', 'group_id']) || typeof request_id !== 'string' || !/^[A-Za-z0-9_-]{16,64}$/.test(request_id)
        || !(group_id === null || typeof group_id === 'string')) throw fail('invalid_request');
      const seen = this.activeGroupReceipts.get(request_id);
      if (seen && seen.group_id !== group_id) throw fail('request_identity_conflict');
      if (group_id !== null && !GROUPS.some((g) => g.id === group_id)) throw fail('library_resource_not_found');
      let ack = seen;
      if (!ack) {
        const hash = [...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`demo-principal:${request_id}`)))].map((b) => b.toString(16).padStart(2, '0')).join('');
        ack = { schema_version: 1, request_id: `mobile-ui-${hash}`, group_id, updated_at: new Date().toISOString() };
        this.activeGroupReceipts.set(request_id, ack);
        this.activeGroup = ack;
      }
      if (flags.lostActiveGroupReplies > 0) { flags.lostActiveGroupReplies -= 1; throw fail('unknown'); }
      return structuredClone(ack);
    },
    // memex.library.groups.tree shape (preorder, paged, bound to a view).
    async groupsTree({ offset = 0, limit = 500, expected_view = null }) {
      await guard();
      if (expected_view && expected_view !== 'gview_1') throw fail('stale_group_view');
      const nodes = preorder().map((g) => ({ id: g.id, name: g.name, kind: g.kind, parent_id: g.parent_id,
        root_group_id: subtreeRoot(g), item_count: membersOf([g.id]).length, subtree_item_count: membersOf(subtree(g.id)).length }));
      const page = nodes.slice(offset, offset + limit);
      return { view: 'gview_1', total: nodes.length, offset, next_offset: offset + page.length < nodes.length ? offset + page.length : null, nodes: page };
    },

    // library_query: text matches title or original Markdown only; facets are source / item_type.
    async libraryQuery({ query, offset = 0, limit = 80, expected_view = null }) {
      await guard();
      if (!(limit >= 1 && limit <= MAX_LIMIT)) throw fail('invalid_request');
      let view = expected_view;
      if (view) {
        const v = views.get(view);
        if (!v || v.generation !== generation) throw fail('stale_query_view');
      } else {
        const q = (query.text || '').trim().toLowerCase();
        const inGroup = query.group_id ? new Set(membersOf(query.include_descendants ? subtree(query.group_id) : [query.group_id]).map((it) => it.id)) : null;
        const filtered = items.filter((it) => (!inGroup || inGroup.has(it.id)) && (!query.origin || (it.origin || 'capture') === query.origin) && (!query.source || it.source === query.source)
          && (!query.item_type || it.type === query.item_type)
          && (!q || it.title.toLowerCase().includes(q) || original(it).toLowerCase().includes(q)));
        const key = { title: 'title', source: 'source', captured_at: 'added', item_type: 'type', added_at: 'addedAt', published_at: 'published' }[query.sort || 'captured_at'];
        if (!key) throw fail('invalid_request');
        const dir = query.descending ? -1 : 1;
        // Nulls last in either direction; ties by item_id ascending (library_query rules).
        filtered.sort((a, b) => (a[key] == null) - (b[key] == null)
          || (a[key] < b[key] ? -dir : a[key] > b[key] ? dir : 0)
          || (a.id < b.id ? -1 : 1));
        view = `view_${(viewSeq += 1)}`;
        views.set(view, { generation, ids: filtered.map((it) => it.id), sources: counts(filtered, 'source'), item_types: counts(filtered, 'type') });
      }
      const v = views.get(view);
      const page = v.ids.slice(offset, offset + limit).map((id) => {
        const it = byId.get(id);
        return { item_id: it.id, revision_id: it.revision_id, title: it.title, source: it.source, item_type: it.type, captured_at: it.added, added_at: it.addedAt ?? null, published_at: it.published ?? null, duration_seconds: it.duration ?? null,
          translated_title: it.translatedTitle ?? null, title_source: it.translatedTitle ? 'title_only' : null };
      });
      const next = offset + page.length;
      return { items: page, total: v.ids.length, offset, next_offset: next < v.ids.length ? next : null, view, generation, sources: v.sources, item_types: v.item_types };
    },

    async itemRead(id) {
      await guard();
      const it = byId.get(id);
      if (!it) throw fail('context_resource_not_found');
      return {
        item_id: it.id, title: it.title, source: it.source, item_type: it.type, captured_at: it.added, translated_title: it.translatedTitle ?? null,
        manual_tags: [...it.tags], revision_id: it.revision_id, translation_status: it.translation,
        summary: summaryOf(it), original_total_bytes: enc.encode(original(it)).length,
        first_segment: firstSegment(enc.encode(original(it))),
      };
    },

    // context_read: original text, UTF-8 half-open byte range, ≤16KiB, both ends on char boundaries.
    async contextRead({ id, expectedRevision, startByte, endByte }) {
      await guard();
      const it = byId.get(id);
      if (!it) throw fail('context_resource_not_found');
      if (expectedRevision !== it.revision_id) throw fail('stale_item_revision');
      const bytes = enc.encode(original(it));
      if (!(startByte >= 0 && endByte > startByte && endByte <= bytes.length && endByte - startByte <= MAX_SEGMENT)) throw fail('invalid_context_range');
      let text;
      try { text = dec.decode(bytes.subarray(startByte, endByte)); } catch { throw fail('invalid_context_range'); }
      return { text, start_byte: startByte, end_byte: endByte, total_bytes: bytes.length, content_sha256: `demo-${bytes.length}`, revision_id: it.revision_id };
    },

    // Derived translation text (library_item_read); separate from original byte coordinates.
    // memex.library.translation.read shape: server pages the translated Markdown on ≤16KiB UTF-8
    // boundaries in its own coordinate space (never original byte offsets). Only succeeded jobs.
    async translationRead({ id, expectedRevision, startByte = 0 }) {
      await guard();
      const it = byId.get(id);
      if (!it) throw fail('context_resource_not_found');
      if (expectedRevision !== it.revision_id) throw fail('stale_item_revision');
      if (it.translation !== 'done') throw fail('translation_not_ready');
      const bytes = enc.encode(it.talkEn ? talkEnZh() : doc(it, it.english ? 'zh' : 'en'));
      let end = Math.min(bytes.length, startByte + MAX_SEGMENT);
      while (end < bytes.length && (bytes[end] & 0xc0) === 0x80) end -= 1;
      return { title: it.english ? (it.translatedTitle || `${it.title}（译）`) : null, text: dec.decode(bytes.subarray(startByte, end)), start_byte: startByte, end_byte: end,
        next_byte: end < bytes.length ? end : null, total_bytes: bytes.length, revision_id: it.revision_id };
    },

    // memex.capture.enqueue / status shape: one job per request_key (exact replay returns the same
    // job). Demo only: nothing is fetched; the stored item is a synthetic placeholder.
    async captureEnqueue({ request_key, url }) {
      await guard();
      if (!jobs.has(request_key)) {
        const id = `job_${(jobSeq += 1).toString(16).padStart(32, '0')}`;
        jobs.set(request_key, { job_id: id, url, started: Date.now(), item_id: null });
        jobsById.set(id, jobs.get(request_key));
      }
      return jobView(jobs.get(request_key));
    },
    async captureStatus({ job_id }) {
      await guard();
      const j = jobsById.get(job_id);
      if (!j) throw fail('context_resource_not_found');
      return jobView(j);
    },
    // Post-ingest translation job view for the item's current revision.
    // memex.library.translation.status shape: {state, reason, revision_id}. Items with no job are
    // not_requested — never reported as skipped or done.
    async translationStatus(item_id) {
      await guard();
      const it = byId.get(item_id);
      if (!it) throw fail('context_resource_not_found');
      const view = (state, reason = null, progress = null) => ({ state, reason, revision_id: it.revision_id,
        // Only an English source gets a (Chinese) translated title; a Chinese one keeps its own title.
        translated_title: it.english && (['succeeded', 'running'].includes(state) || progress) ? (it.translatedTitle || `${it.title}（译）`) : null, progress });
      flags.trStatusReads += 1;
      const ov = flags.trStatus[item_id];
      if (ov?.startsWith('error:')) throw fail(ov.slice(6));
      if (ov === 'bump') { flags.trStatus[item_id] = 'running03'; bump(it); }
      if (ov === 'running03' || ov === 'bump') return view('running', null, { done_batches: 0, total_batches: 3 });
      if (ov === 'unavailable') return view('unavailable');
      if (ov === 'ahead') return { ...view('running', null, { done_batches: 0, total_batches: 3 }), revision_id: `${it.revision_id}_ahead` };
      if (requested.has(item_id)) {
        const done = Math.floor((Date.now() - requested.get(item_id)) / 1500);
        if (done < 1) return view('queued');
        if (done <= BATCHES) return view('running', null, { done_batches: done - 1, total_batches: BATCHES });
        it.translation = 'done'; requested.delete(item_id);
      }
      if (!it.capturedAt) {
        if (it.translation === 'running') return view('running', null, { done_batches: 2, total_batches: BATCHES });
        return view(it.translation === 'done' ? 'succeeded' : 'not_requested');
      }
      if (!it.english) return view('skipped', 'already_chinese');
      const t = Date.now() - it.capturedAt;
      const state = t < 2500 ? 'queued' : t < 6000 ? 'running' : 'succeeded';
      it.translation = state === 'succeeded' ? 'done' : 'running';
      return view(state);
    },

    // memex.library.translation.request: explicit 翻译此文 for the current revision.
    async translationRequest(item_id) {
      await guard(); calls.push('translationRequest');
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (it.longZh || it.guji || it.talk) return { state: 'not_requested', reason: 'source_is_chinese', revision_id: it.revision_id, translated_title: null, progress: null, ai_minutes: await this.minutesStatus(item_id) };
      if (it.translation !== 'done' && !requested.has(item_id)) requested.set(item_id, Date.now());
      const t = await this.translationStatus(item_id);
      return { ...t, ai_minutes: await this.minutesStatus(item_id) };
    },
    // memex.library.minutes.status / .request: {state, reason, text}; text only when succeeded.
    async minutesStatus(item_id) {
      await guard();
      if (!byId.has(item_id)) throw fail('library_resource_not_found');
      const m = minutes.get(item_id);
      if (!m) return { state: 'not_requested', reason: null, text: null };
      if (m.startedAt) {
        const t = Date.now() - m.startedAt;
        if (t < 1500) return { state: 'queued', reason: null, text: null };
        if (t < 4500) return { state: 'running', reason: null, text: null };
        Object.assign(m, { state: 'succeeded', text: minutesOf(byId.get(item_id)), startedAt: null });
      }
      return { state: m.state, reason: m.reason ?? null, text: m.state === 'succeeded' ? m.text : null };
    },
    async minutesRequest(item_id) {
      await guard();
      if (!byId.has(item_id)) throw fail('library_resource_not_found');
      const m = minutes.get(item_id);
      if (m?.state !== 'succeeded' && !m?.startedAt) minutes.set(item_id, { state: 'queued', startedAt: Date.now() });
      return this.minutesStatus(item_id);
    },

    // insight read / insight_request shape: {highlights, quotes, fact_check}, each {state, reason, items}.
    async insightsStatus(item_id) {
      await guard();
      const it = byId.get(item_id);
      if (!it) throw fail('library_resource_not_found');
      if (!insights.has(item_id)) insights.set(item_id, preset(item_id));
      const cur = insights.get(item_id); const out = {};
      for (const [k, v] of Object.entries(cur)) {
        if (v.startedAt) {
          const t = Date.now() - v.startedAt;
          // demo-3's 金句 keeps failing so the failed view (reason + 重试) can be reviewed.
          if (t >= 3500) cur[k] = item_id === 'demo-3' && k === 'quotes' ? { state: 'failed', reason: 'provider_timeout', items: [] } : allInsights(it)[k];
          else { out[k] = { state: t < 1200 ? 'queued' : 'running', reason: null, items: [] }; continue; }
        }
        out[k] = cur[k];
      }
      return out;
    },
    async insightsRequest(item_id, kinds) {
      calls.push('insightsRequest');
      await this.insightsStatus(item_id);
      const cur = insights.get(item_id);
      for (const k of kinds) if (cur[k].state !== 'succeeded' && !cur[k].startedAt) cur[k] = { state: 'queued', startedAt: Date.now() };
      return this.insightsStatus(item_id);
    },

    async updateItemMetadata({ item_id, expected_revision_id, title, manual_tags }) {
      await guard();
      await delay(300);
      const it = byId.get(item_id);
      if (!it) throw fail('context_resource_not_found');
      if (flags.conflictNextSave) {
        flags.conflictNextSave = false;
        bump(it);
        it.title = `${it.title.replace(/（Mac 上已更新）$/, '')}（Mac 上已更新）`;
      }
      if (it.revision_id !== expected_revision_id) throw fail('stale_item_revision');
      const previous = it.revision_id;
      Object.assign(it, { title, tags: [...manual_tags] });
      bump(it);
      // Simulates a reply lost after the Mac committed the write: the outcome is unknown to the client.
      if (flags.unknownNextSave) { flags.unknownNextSave = false; throw fail('unknown'); }
      return { previous_revision_id: previous, revision_id: it.revision_id, revision_number: it.revision_number };
    },
  };

  function jobView(j) {
    const t = Date.now() - j.started;
    const state = t < 1200 ? 'queued' : t < 3500 ? 'leased' : t < 5000 ? 'artifact_ready' : 'succeeded';
    if (state === 'succeeded' && !j.item_id) {
      const u = new URL(j.url);
      const slug = decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() || u.hostname);
      const id = `demo-cap-${jobSeq}-${items.length + 1}`;
      const it = {
        id, title: `（演示）${slug}`, source: u.origin, type: 'webpage', addedAt: new Date().toISOString().slice(0, 16).replace('T', ' '), published: null, groups: [],
        added: new Date().toISOString().slice(0, 16).replace('T', ' '), unread: true,
        translation: 'running', tags: [], revision_number: 1, revision_id: rev(id, 1),
        capturedAt: Date.now(), english: !/[\u3400-\u9fff]/.test(slug),
      };
      items.unshift(it); byId.set(id, it); generation += 1;
      j.item_id = id;
    }
    return { job_id: j.job_id, state, item_id: j.item_id };
  }

  // Synthetic stages over ~3 s; the commit happens once, on the first read after it is due.
  function xlStage(j) { const t = Date.now() - j.started; return t < 600 ? 'queued' : t < 2400 ? 'translating' : t < 3000 ? 'committing' : 'done'; }
  // Synthetic output: fixed demo text, labelled as such; never a model answer.
  async function xlSettle(j) {
    if (j.done || xlStage(j) !== 'done') return;
    if (j.outcome !== 'success') { j.done = { state: j.outcome === 'failed' ? 'failed' : 'outcome_unknown', error_code: j.outcome === 'failed' ? 'reader_translation_unavailable' : 'provider_outcome_unknown' }; return; }
    const oc = BOOK.original.chapters.find((c) => c.key === j.chapter);
    const md = `# ${oc.title}（演示译文）\n\n这是演示模式里的合成 AI 译文，没有调用任何模型；真实译文由 Mac 上保存的翻译模型生成。\n\n（合成演示文本，不是任何真实书籍的译文。）`;
    const bytes = enc.encode(md); const sha256 = await sha256hex(bytes);
    const meta = { version: await sha256hex(enc.encode(`demo-xl:${j.chapter}:${j.job_id}`)), item_id: j.item_id, revision_id: j.revision_id, original_chapter: j.chapter,
      source_sha256: await sha256hex(chapterBytes('original', j.chapter)), target_language: 'zh-CN', sha256, bytes: bytes.length, provenance: 'machine_translation', route: { ...XL_ROUTE } };
    xlDone.set(j.chapter, { meta, bytes });
    j.done = { state: 'succeeded', error_code: null, translation: meta };
  }
  function xlJobView(j) {
    const d = j.done; const stage = d ? 'done' : xlStage(j) === 'done' ? 'committing' : xlStage(j);
    const simulated = d && d.state !== 'succeeded';
    return { job_id: j.job_id, request_id: j.request_id, item_id: j.item_id, revision_id: j.revision_id, chapter: j.chapter, operation: 'chapter_translation',
      state: d ? d.state : stage === 'queued' ? 'queued' : 'running', stage, error_code: d?.error_code ?? null,
      failure_stage: simulated ? 'translating' : null, failure_reason: simulated ? d.error_code : null, translation: d?.translation ?? null,
      completed_blocks: d?.state === 'succeeded' ? 2 : stage === 'committing' ? 2 : stage === 'translating' ? 1 : 0, total_blocks: 2,
      budget: { ...XL_BUDGET },
      usage: simulated ? { requests_dispatched: 1, reported_total_tokens: 0, unknown_usage_requests: 1, dispatches: [{ input_tokens: null, output_tokens: null, total_tokens: null }], input_tokens: null, output_tokens: null }
        : { requests_dispatched: 0, reported_total_tokens: 0, unknown_usage_requests: 0, dispatches: [], input_tokens: 0, output_tokens: 0 },
      elapsed_ms: Math.min(Date.now() - j.started, 3000) };
  }
  function alignJobView(j) {
    const t = Date.now() - j.started;
    const stage = t < 600 ? 'queued' : t < 1400 ? 'formatting' : t < 2200 ? 'matching' : t < 3000 ? 'committing' : 'done';
    if (stage === 'done' && !j.done) {
      // The demo has no repaired text for an owner-edited chapter: such a repair fails honestly.
      if (j.fail || alignStale.has(j.chapter)) j.done = { state: 'failed', stage: 'matching', error_code: 'reader_repair_failed' };
      else {
        const it = byId.get(j.item_id);
        alignRepaired.add(j.chapter); alignGen.set(j.chapter, (alignGen.get(j.chapter) || 0) + 1);
        // A formatting change is a new item revision; an index-only change keeps the revision.
        if (DEMO_BOOK_REPAIRED.original[j.chapter] && !textRepaired.has(j.chapter)) { keepRevision(it); textRepaired.add(j.chapter); buildBook(); bump(it); }
        const rows = alignRows(j.chapter);
        j.done = { state: 'succeeded', stage: 'done', error_code: null, result_revision_id: it.revision_id, alignment_version: alignVersion(j.chapter),
          alignment_status: rows.some((r) => r[3] === 'unresolved') ? 'partial' : 'aligned' };
      }
    }
    const d = j.done;
    return { job_id: j.job_id, request_id: j.request_id, item_id: j.item_id, revision_id: j.revision_id, chapter: j.chapter,
      state: d ? d.state : stage === 'queued' ? 'queued' : 'running', stage: d ? d.stage : stage,
      result_revision_id: d?.result_revision_id ?? null, alignment_version: d?.alignment_version ?? null, alignment_status: d?.alignment_status ?? null,
      error_code: d?.error_code ?? null, budget: { ...JOB_BUDGET },
      // Synthetic: no provider is ever called, so nothing was dispatched.
      usage: { requests_dispatched: 0, reported_total_tokens: 0, unknown_usage_requests: 0 }, elapsed_ms: Math.min(t, 3000) };
  }

  function subtreeRoot(g) { let n = g; while (n.parent_id) n = GROUPS.find((x) => x.id === n.parent_id); return n.id; }

  function bump(it) {
    keepRevision(it);
    it.revision_number += 1;
    it.revision_id = rev(it.id, it.revision_number);
    generation += 1;
  }
}
