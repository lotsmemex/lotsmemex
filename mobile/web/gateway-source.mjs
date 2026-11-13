// Private Mac gateway source. Same interface as demo-source.mjs, backed by the encrypted RPC
// channel from remote/web (openSecureChannel → { request(id, method, params) }).
// Scope, sensitivity ceiling and expiry are granted by the Mac session and are never sent by this
// client. Item chat runs on the Mac's chat route; translation and AI minutes are explicit requests.
//
// Error policy:
// - pure reads (RECONNECT_READS) whose channel drops (socket closed, not connected, timeout before
//   the reply) wait for the channel to come back and replay the IDENTICAL params, bounded by
//   RECONNECT_CYCLES drops within RECONNECT_WINDOW_MS; exhausted → `unavailable`;
// - request not sent (no channel), or transport failure / timeout on any other read → `unavailable`;
// - writes and model-dispatching requests are never replayed: a drop after sending is `unknown`;
// - transport failure / timeout after sending metadata.update → `unknown` (read back first,
//   never resend blindly);
// - gateway error codes pass through unchanged (stale_query_view, stale_item_revision, ...).

const fail = (code) => Object.assign(new Error(code), { code });
const isObject = (v) => v && typeof v === 'object' && !Array.isArray(v);
// result.job of align_request / align_status, or null when it does not match the frozen shape.
const strOrNull = (v) => v === null || (typeof v === 'string' && v.length > 0);
function alignJob(r) {
  const j = r?.job;
  if (!isObject(j) || ![j.job_id, j.request_id, j.item_id, j.revision_id, j.chapter].every((v) => typeof v === 'string' && v)
    || !['queued', 'running', 'succeeded', 'failed', 'outcome_unknown'].includes(j.state)
    || !['queued', 'formatting', 'matching', 'committing', 'done'].includes(j.stage)
    || !strOrNull(j.result_revision_id) || !strOrNull(j.alignment_version) || !(j.alignment_status === null || ['aligned', 'partial', 'none'].includes(j.alignment_status))
    || !strOrNull(j.error_code) || (j.state === 'succeeded' && (!j.result_revision_id || !j.alignment_version))) return null;
  return { job_id: j.job_id, request_id: j.request_id, item_id: j.item_id, revision_id: j.revision_id, chapter: j.chapter, state: j.state, stage: j.stage,
    result_revision_id: j.result_revision_id, alignment_version: j.alignment_version, alignment_status: j.alignment_status, error_code: j.error_code,
    budget: isObject(j.budget) ? j.budget : null, usage: isObject(j.usage) ? j.usage : null, elapsed_ms: Number.isSafeInteger(j.elapsed_ms) ? j.elapsed_ms : null };
}

// reader_chapter_translation_v1 (frozen extension): projection metadata, quote, usage and job.
const hex64 = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
const count = (v) => Number.isSafeInteger(v) && v >= 0;
const countOrNull = (v) => (count(v) ? v : null);
const str = (v) => typeof v === 'string' && v.length > 0;
// Query-row publication day + duration (memex.library_query.md "Publication day and duration summary"):
// a real YYYY-MM-DD day (year 0001–9999) and positive u32 seconds. Omitted (older daemon) or wrong-typed
// values are unknown (null); they never fail the page.
const pubDay = (v) => (typeof v === 'string' && /^(?!0000)\d{4}-\d{2}-\d{2}$/.test(v)
  && !Number.isNaN(Date.parse(`${v}T00:00:00Z`)) && new Date(`${v}T00:00:00Z`).toISOString().slice(0, 10) === v ? v : null);
const durSecs = (v) => (Number.isInteger(v) && v > 0 && v <= 0xFFFFFFFF ? v : null);
function xlRoute(r) {
  if (!isObject(r) || !str(r.provider_id) || !str(r.model_id) || !(r.reasoning_effort == null || typeof r.reasoning_effort === 'string')) return null;
  return { provider_id: r.provider_id, model_id: r.model_id, reasoning_effort: r.reasoning_effort || null };
}
function xlProjection(t) {
  const route = isObject(t) ? xlRoute(t.route) : null;
  if (!route || !hex64(t.version) || !hex64(t.sha256) || !hex64(t.source_sha256) || !count(t.bytes) || !str(t.item_id) || !str(t.revision_id)
    || !str(t.original_chapter) || t.provenance !== 'machine_translation' || t.target_language !== 'zh-CN') return null;
  return { version: t.version, item_id: t.item_id, revision_id: t.revision_id, original_chapter: t.original_chapter, source_sha256: t.source_sha256,
    target_language: t.target_language, sha256: t.sha256, bytes: t.bytes, provenance: t.provenance, route };
}
const BUDGET_KEYS = ['max_requests', 'max_input_bytes_per_request', 'max_completion_tokens_per_request', 'max_total_input_bytes', 'max_total_completion_tokens'];
const xlBudget = (b) => (isObject(b) && BUDGET_KEYS.every((k) => count(b[k])) ? Object.fromEntries(BUDGET_KEYS.map((k) => [k, b[k]])) : null);
// Usage as reported: a missing split stays null (never derived from totals); older replies may
// omit the per-dispatch fields entirely, which also reads as null.
function xlUsage(u) {
  if (!isObject(u)) return null;
  return { requests_dispatched: countOrNull(u.requests_dispatched), reported_total_tokens: countOrNull(u.reported_total_tokens),
    unknown_usage_requests: countOrNull(u.unknown_usage_requests),
    dispatches: Array.isArray(u.dispatches) ? u.dispatches.map((d) => ({ input_tokens: countOrNull(d?.input_tokens), output_tokens: countOrNull(d?.output_tokens), total_tokens: countOrNull(d?.total_tokens) })) : null,
    input_tokens: countOrNull(u.input_tokens), output_tokens: countOrNull(u.output_tokens) };
}
function xlJob(r) {
  const j = r?.job;
  if (!isObject(j) || ![j.job_id, j.request_id, j.item_id, j.revision_id, j.chapter].every(str) || j.operation !== 'chapter_translation'
    || !['queued', 'running', 'succeeded', 'failed', 'outcome_unknown'].includes(j.state)
    || !['queued', 'translating', 'committing', 'done'].includes(j.stage) || !strOrNull(j.error_code)) return null;
  const translation = j.translation == null ? null : xlProjection(j.translation);
  if ((j.translation != null && !translation) || (j.state === 'succeeded' && !translation)) return null;
  return { job_id: j.job_id, request_id: j.request_id, item_id: j.item_id, revision_id: j.revision_id, chapter: j.chapter, state: j.state, stage: j.stage,
    error_code: j.error_code, failure_stage: j.failure_stage ?? null, failure_reason: j.failure_reason ?? null, translation,
    completed_blocks: countOrNull(j.completed_blocks), total_blocks: countOrNull(j.total_blocks),
    budget: xlBudget(j.budget), usage: xlUsage(j.usage), elapsed_ms: countOrNull(j.elapsed_ms) };
}

// reader_chapter_edit_v1 receipt: {item_id,request_id,revision_id,side,chapter,chapter_sha256,changed}.
function editReceipt(r) {
  if (!isObject(r) || ![r.item_id, r.request_id, r.revision_id, r.chapter].every(str) || !['original', 'translation'].includes(r.side)
    || !hex64(r.chapter_sha256) || typeof r.changed !== 'boolean') return null;
  return { item_id: r.item_id, request_id: r.request_id, revision_id: r.revision_id, side: r.side, chapter: r.chapter, chapter_sha256: r.chapter_sha256, changed: r.changed };
}

// library_loading: the Mac's single background query slot is busy (another read, an edit, an index
// rebuild). It never means the data is bad. Reads below are retried with the SAME params (revision,
// hash, version) after these pauses; then the caller shows 「Mac 正忙，稍后重试」 and keeps its caches.
// Only pure, side-effect-free reads (reader_v1 reconnect contract): translate_preview and
// format_preview create a quote / preview and are NEVER resent automatically (not on
// library_loading, not on a transport drop); the user asks again from the existing error state.
export const BUSY_READS = new Set(['memex.library.query', 'memex.library.groups.tree', 'memex.library.reader.toc', 'memex.library.reader.chunk',
  'memex.library.reader.alignment', 'memex.library.reader.align_status', 'memex.library.reader.revision_chunk',
  'memex.library.reader.translate_status', 'memex.library.reader.translate_chunk',
  'memex.library.reader.translate_metadata', 'memex.library.reader.translate_alignment', 'memex.library.reader.format_targets', 'memex.library.reader.format_status',
  'memex.library.reader.format_results', 'memex.library.reader.format_changes',
  // B2 reader state and the manual-mark ledger: pure reads (save / edit are writes). reader.resume is a
  // read too, but this phone does not call it yet, so it is not in the replay sets.
  'memex.library.reader.state', 'memex.library.reader.annotations.list',
  // 阅读记录: list and stats are pure reads (edit / timing_start / timing_report are writes).
  'memex.library.reader.passes.list', 'memex.library.reader.passes.stats']);
export const BUSY_BACKOFF_MS = [300, 600, 1200, 2400, 4000, 4000];
// Transport drops (the relay closes the phone socket when the Mac leg goes away, 4002 / 4005): the
// same pure reads, plus the item reads that open the reader, wait for the channel and are replayed
// unchanged. Never a write, never a request that may dispatch a model or create a quote / preview.
export const RECONNECT_READS = new Set([...BUSY_READS, 'memex.library.item.get', 'memex.library.context.read', 'memex.session.info',
  'memex.library.image.read', 'memex.library.item_cover_batch', 'memex.library.item_cover_chunk', 'memex.library.conversation.read', 'memex.library.trash.list', 'memex.library.translation.read',
  'memex.library.translation.status', 'memex.library.minutes.status', 'memex.library.insights.status', 'memex.capture.status']);
export const RECONNECT_CYCLES = 2;
export const RECONNECT_WINDOW_MS = 20000;

// 格式整理 (Unified content formatting v1) shapes.
function fmtTarget(t) {
  if (isObject(t) && t.kind === 'body') return { kind: 'body' };
  if (isObject(t) && t.kind === 'chapter' && ['original', 'translation'].includes(t.side) && str(t.chapter)) return { kind: 'chapter', side: t.side, chapter: t.chapter };
  return null;
}
function fmtQuote(q) {
  const c = q?.counts; const b = q?.budget;
  const route = q?.route == null ? null : xlRoute(q.route);
  const cost = q?.estimated_cost;
  if (!isObject(q) || !str(q.quote_id) || !count(q.expires_at) || !isObject(c) || !['items', 'targets', 'eligible', 'skipped', 'private_targets'].every((k) => count(c[k]))
    || !isObject(b) || !['max_requests', 'max_input_bytes', 'max_completion_tokens'].every((k) => count(b[k])) || (q.route != null && !route)
    || !(cost == null || (isObject(cost) && str(cost.amount) && /^[A-Z]{3}$/.test(cost.currency || '')))) return null;
  const reasons = isObject(c.skipped_by_reason) ? Object.fromEntries(Object.entries(c.skipped_by_reason).filter(([, v]) => count(v))) : {};
  return { quote_id: q.quote_id, expires_at: q.expires_at, selection: q.selection ?? null, policy_version: q.policy_version ?? null, route,
    counts: { items: c.items, targets: c.targets, eligible: c.eligible, skipped: c.skipped, private_targets: c.private_targets, skipped_by_reason: reasons },
    budget: { max_requests: b.max_requests, max_input_bytes: b.max_input_bytes, max_completion_tokens: b.max_completion_tokens },
    estimated_cost: cost ? { amount: cost.amount, currency: cost.currency } : null };
}
const FMT_COUNTS = ['total', 'queued', 'running', 'succeeded', 'failed', 'outcome_unknown', 'skipped'];
function fmtBatch(b) {
  if (!isObject(b) || !str(b.batch_id) || !str(b.request_id) || !['queued', 'running', 'paused', 'done'].includes(b.state)
    || !isObject(b.counts) || !FMT_COUNTS.every((k) => count(b.counts[k]))) return null;
  return { batch_id: b.batch_id, request_id: b.request_id, state: b.state, counts: Object.fromEntries(FMT_COUNTS.map((k) => [k, b.counts[k]])),
    usage: xlUsage(b.usage), error_code: b.error_code ?? null };
}
function fmtRow(r) {
  const target = isObject(r) ? fmtTarget(r.target) : null;
  if (!target || !str(r.target_id) || !str(r.item_id) || !['queued', 'running', 'succeeded', 'failed', 'outcome_unknown', 'skipped'].includes(r.state)) return null;
  const j = r.job; const f = r.formatting;
  const job = isObject(j) ? { job_id: j.job_id, request_id: j.request_id, state: j.state, stage: j.stage ?? null, revision_id: j.revision_id ?? null,
    result_revision_id: j.result_revision_id ?? null, error_code: j.error_code ?? null, failure_stage: j.failure_stage ?? null, failure_reason: j.failure_reason ?? null,
    usage: xlUsage(j.usage), elapsed_ms: countOrNull(j.elapsed_ms) } : null;
  const formatting = isObject(f) && typeof f.changed === 'boolean' && hex64(f.changes_version) ? { status: f.status, changed: f.changed, applied_count: countOrNull(f.applied_count),
    correction_count: countOrNull(f.correction_count), deleted_blocks: countOrNull(f.deleted_blocks), unresolved_count: countOrNull(f.unresolved_count),
    checked_bytes: countOrNull(f.checked_bytes), total_bytes: countOrNull(f.total_bytes), changes_version: f.changes_version } : null;
  if (r.state === 'succeeded' && (!formatting || !job)) return null;
  return { target_id: r.target_id, item_id: r.item_id, revision_id: r.revision_id ?? null, target, state: r.state, reason: r.reason ?? null, job, formatting };
}

// 条目封面 (item_cover_v1) DTOs: exact keys, or the whole reply is invalid_response.
const exactKeys = (o, keys) => isObject(o) && Object.keys(o).length === keys.length && keys.every((k) => Object.hasOwn(o, k));
const COVER_MEDIA = new Set(['image/jpeg', 'image/png', 'image/webp']);
const COVER_MAX_BYTES = 5 * 1024 * 1024;
const SENSITIVITY = ['public', 'internal', 'private', 'restricted'];
const pos = (v) => Number.isSafeInteger(v) && v > 0;
function coverRaster(r, thumb) {
  if (!exactKeys(r, ['sha256', 'media_type', 'width', 'height', 'byte_size']) || !hex64(r.sha256) || !COVER_MEDIA.has(r.media_type)
    || !pos(r.width) || !pos(r.height) || !pos(r.byte_size) || r.byte_size > COVER_MAX_BYTES || r.width * r.height > 20_000_000
    || (thumb && (r.media_type !== 'image/png' || Math.max(r.width, r.height) > 320))) throw fail('invalid_response');
  return { sha256: r.sha256, media_type: r.media_type, width: r.width, height: r.height, byte_size: r.byte_size };
}
// Cover source: the podcast shape, or the WeChat article shape (article_url/image_url/html_sha256).
function coverSource(s) {
  if (exactKeys(s, ['episode_url', 'youtube_video_id', 'image_url', 'rss_sha256'])
    && str(s.episode_url) && str(s.youtube_video_id) && str(s.image_url) && hex64(s.rss_sha256))
    return { episode_url: s.episode_url, youtube_video_id: s.youtube_video_id, image_url: s.image_url, rss_sha256: s.rss_sha256 };
  if (exactKeys(s, ['article_url', 'image_url', 'html_sha256']) && str(s.article_url) && str(s.image_url) && hex64(s.html_sha256))
    return { article_url: s.article_url, image_url: s.image_url, html_sha256: s.html_sha256 };
  return null;
}
function coverOf(c, item_id) {
  const source = coverSource(c?.source);
  if (!exactKeys(c, ['item_id', 'version', 'source_revision_id', 'source', 'raster', 'thumbnail', 'sensitivity']) || c.item_id !== item_id || !hex64(c.version)
    || !str(c.source_revision_id) || !SENSITIVITY.includes(c.sensitivity) || !source) throw fail('invalid_response');
  return { item_id: c.item_id, version: c.version, source_revision_id: c.source_revision_id, source,
    raster: coverRaster(c.raster, false), thumbnail: coverRaster(c.thumbnail, true), sensitivity: c.sensitivity };
}

// onReconnect(+1 | -1): a foreground read started / stopped waiting for the channel to come back
// (the UI shows 正在重新连接 Mac… meanwhile, never a failure).
export function createGatewaySource({ channel, onReconnect = () => {} }) {
  let seq = 0;
  // Foreground reads in flight: background prefetch waits until there are none (idle()).
  let foreground = 0; let idleWaiters = [];
  const settleIdle = () => { if (foreground === 0) { const w = idleWaiters; idleWaiters = []; for (const r of w) r(); } };

  // `background` (prefetch): no library_loading retry and no reconnect replay; the first busy reply
  // or drop is reported to the caller, which abandons the prefetch.
  // `array`: the result is a bare JSON array (item_cover_batch), not an object.
  async function call(method, params, { write = false, timeoutMs, background = false, array = false } = {}) {
    let response;
    // Writes and model-dispatching requests are never retried here: a write that meets
    // library_loading is reported as is and the caller keeps its request id.
    const busyRead = !write && !background && BUSY_READS.has(method);
    const replayable = !write && !background && RECONNECT_READS.has(method) && typeof channel.waitReady === 'function';
    let drops = 0; let deadline = 0;
    const fg = !write && !background && BUSY_READS.has(method);
    if (fg) foreground += 1;
    try {
      for (let attempt = 0; ; attempt += 1) {
        try {
          response = await channel.request(`rpc_mobile_${++seq}`, method, params, timeoutMs ? { timeoutMs } : undefined);
        } catch (e) {
          // A write that may have been sent is unknown; it is never resent here.
          if (write) throw fail(e?.code !== 'unavailable' ? 'unknown' : 'unavailable');
          if (!replayable || drops >= RECONNECT_CYCLES) throw fail('unavailable');
          drops += 1;
          if (!deadline) deadline = Date.now() + RECONNECT_WINDOW_MS;
          // Only while the channel is down: the replay itself is an ordinary request.
          onReconnect(1);
          let back = false;
          try { back = await channel.waitReady(deadline - Date.now()); } finally { onReconnect(-1); }
          if (!back) throw fail('unavailable');
          attempt -= 1; // the replay is not a busy retry
          continue;
        }
        if (!busyRead || response?.error?.code !== 'library_loading' || attempt >= BUSY_BACKOFF_MS.length) break;
        await new Promise(resolve => setTimeout(resolve, BUSY_BACKOFF_MS[attempt]));
      }
    } finally {
      if (fg) { foreground -= 1; settleIdle(); }
    }
    if (!isObject(response)) throw fail(write ? 'unknown' : 'invalid_response');
    if (response.error) throw fail(typeof response.error.code === 'string' ? response.error.code : 'invalid_response');
    if (array ? !Array.isArray(response.result) : !isObject(response.result)) throw fail(write ? 'unknown' : 'invalid_response');
    return response.result;
  }

  return {
    kind: 'gateway',
    flags: {},
    // Resolves when no foreground read is in flight (the Mac serves one read at a time).
    idle: () => (foreground === 0 ? Promise.resolve() : new Promise((r) => idleWaiters.push(r))),

    async imageRead({ item_id, expected_revision_id, target, signal }) {
      const match = /^attachments\/(?:sha256-)?([a-f0-9]{64})\.(png|jpe?g|gif|webp)$/.exec(target);
      if (!match) throw fail('image_resource_not_found');
      const media = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp' }[match[2]];
      const chunks = []; let offset = 0; let total = null;
      while (true) {
        if (signal?.aborted) throw fail('cancelled');
        const r = await call('memex.library.image.read', { item_id, expected_revision_id, target, offset });
        if (signal?.aborted) throw fail('cancelled');
        if (r.item_id !== item_id || r.revision_id !== expected_revision_id || r.target !== target
          || r.sha256 !== match[1] || r.media_type !== media || r.offset !== offset
          || !Number.isSafeInteger(r.total_bytes) || r.total_bytes < 1 || r.total_bytes > 64 * 1024 * 1024
          || total !== null && total !== r.total_bytes || typeof r.data_base64 !== 'string' || r.data_base64.length > 131072) throw fail('invalid_response');
        total = r.total_bytes;
        let bytes;
        try { bytes = Uint8Array.from(atob(r.data_base64), c => c.charCodeAt(0)); } catch { throw fail('invalid_response'); }
        if (!bytes.length || bytes.length > 96 * 1024 || offset + bytes.length > total) throw fail('invalid_response');
        offset += bytes.length; chunks.push(bytes);
        if (r.next_offset !== (offset < total ? offset : null)) throw fail('invalid_response');
        if (offset === total) break;
      }
      const bytes = new Uint8Array(total); let at = 0;
      for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length; }
      const hash = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), b => b.toString(16).padStart(2, '0')).join('');
      if (hash !== match[1]) throw fail('invalid_response');
      return new Blob([bytes], { type: media });
    },

    // 条目封面 (item_cover_v1; the app calls these only when session.info.item_cover_v1 is 'available').
    // batch → the ordered nullable Cover array itself (one position per requested id, ≤ 50); chunk → one
    // ≤ 96 KiB piece of the original (thumbnail=false) or the PNG thumbnail (longest edge ≤ 320) of exactly
    // that version. Every field is checked here; the app assembles the pieces and verifies the SHA-256.
    async itemCoverBatch({ item_ids }) {
      if (!Array.isArray(item_ids) || item_ids.length > 50) throw fail('invalid_cover_request');
      const r = await call('memex.library.item_cover_batch', { item_ids: [...item_ids] }, { array: true });
      if (r.length !== item_ids.length) throw fail('invalid_response');
      return r.map((c, i) => (c === null ? null : coverOf(c, item_ids[i])));
    },
    async itemCoverChunk({ item_id, version, thumbnail, offset }) {
      const r = await call('memex.library.item_cover_chunk', { item_id, version, thumbnail, offset });
      if (!exactKeys(r, ['media_type', 'sha256', 'total_bytes', 'offset', 'next_offset', 'data_base64']) || !COVER_MEDIA.has(r.media_type)
        || (thumbnail && r.media_type !== 'image/png') || !hex64(r.sha256) || !Number.isSafeInteger(r.total_bytes) || r.total_bytes < 1 || r.total_bytes > COVER_MAX_BYTES
        || r.offset !== offset || offset >= r.total_bytes || typeof r.data_base64 !== 'string' || r.data_base64.length > 131072
        || !(r.next_offset === null || (Number.isSafeInteger(r.next_offset) && r.next_offset > offset && r.next_offset < r.total_bytes))) throw fail('invalid_response');
      return { media_type: r.media_type, sha256: r.sha256, total_bytes: r.total_bytes, offset: r.offset, next_offset: r.next_offset, data_base64: r.data_base64 };
    },

    // memex.library.query → QueryPage (library_query): items carry revision_id, captured_at may be null.
    async libraryQuery({ query, offset = 0, limit = 80, expected_view = null }) {
      const r = await call('memex.library.query', {
        query: { text: query.text || '', source: query.source ?? null, item_type: query.item_type ?? null, sort: query.sort, descending: !!query.descending,
          // groups_v1 node filter: both keys or neither (the router rejects one alone).
          ...(query.group_id ? { group_id: query.group_id, include_descendants: !!query.include_descendants } : {}),
          // PROPOSED (not served yet): own captures vs imports; sent only when session.info says so.
          ...(query.origin ? { origin: query.origin } : {}) },
        offset, limit, expected_view,
      });
      return {
        items: r.items.map((it) => ({ item_id: it.item_id, revision_id: it.revision_id, title: it.title, source: it.source, item_type: it.item_type, captured_at: it.captured_at ?? null,
          added_at: it.added_at ?? null, published_at: pubDay(it.published_at), duration_seconds: durSecs(it.duration_seconds),
          // Candidate (2026-11-03): optional title translation, shown first in the list and reader.
          translated_title: typeof it.translated_title === 'string' ? it.translated_title : null, title_source: it.title_source ?? null })),
        total: r.total, offset: r.offset, next_offset: r.next_offset ?? null,
        view: r.view, generation: r.generation, sources: r.sources || [], item_types: r.item_types || [],
      };
    },

    // memex.library.item.get → bounded header + first original segment. item_type / captured_at
    // are not part of it; the UI carries them from the query row (unknown stays unknown).
    async itemRead(id) {
      const r = await call('memex.library.item.get', { item_id: id });
      return {
        item_id: r.item_id, title: r.title, source: r.source_url ?? null, item_type: null, captured_at: null,
        manual_tags: r.manual_tags || [], revision_id: r.revision_id, sensitivity: r.sensitivity,
        translation_status: null, summary: null,
        original_total_bytes: r.total_bytes, content_sha256: r.content_sha256,
        first_segment: typeof r.markdown === 'string' ? { text: r.markdown, end_byte: r.next_byte ?? r.total_bytes } : null,
      };
    },

    // memex.library.context.read: existing context_read contract, strict UTF-8 half-open byte range ≤16KiB.
    // Request is exactly {item_id, expected_revision_id, start_byte, end_byte}; the server returns its
    // verified evidence object as response.input (revision_sha256 / evidence_ids), which the client keeps.
    async contextRead({ id, expectedRevision, startByte, endByte }) {
      const r = await call('memex.library.context.read', { item_id: id, expected_revision_id: expectedRevision, start_byte: startByte, end_byte: endByte });
      return { text: r.content_markdown, start_byte: r.start_byte, end_byte: r.end_byte, total_bytes: r.total_bytes, content_sha256: r.content_sha256, revision_id: r.input?.revision_id ?? expectedRevision, evidence: r.input ?? null };
    },

    // Link capture through the existing capture queue. request_key makes enqueue idempotent, so an
    // unknown outcome is resolved by sending the same request_key again (exact replay → same job).
    // PENDING gateway allowlist: memex.capture.enqueue / memex.capture.status for the scoped session.
    async captureEnqueue({ request_key, url }) {
      return call('memex.capture.enqueue', { request_key, url }, { write: true });
    },
    async captureStatus({ job_id }) { return call('memex.capture.status', { job_id }); },
    // PROPOSED (not final): body-free job view for the item's current revision,
    // { state: queued|running|succeeded|failed|skipped|not_requested|unavailable, reason, revision_id }.
    async translationStatus(item_id) {
      let r;
      // A gateway that does not offer the method rejects it as invalid_request: report unavailable.
      try { r = await call('memex.library.translation.status', { item_id }); } catch (e) {
        if (e.code === 'invalid_request') return { state: 'unavailable', reason: null, revision_id: null };
        throw e;
      }
      return { state: r.state, reason: r.reason ?? null, revision_id: r.revision_id,
        translated_title: r.translated_title ?? null, progress: r.progress ?? null };
    },
    // Explicit 翻译此文 / 生成纪要 for the item's current revision (Mac-side jobs, `capture` grant).
    // A lost reply is `unknown`: the UI reads the status back and never resends by itself.
    async translationRequest(item_id) {
      const r = await call('memex.library.translation.request', { item_id }, { write: true });
      return { state: r.state, reason: r.reason ?? null, revision_id: r.revision_id,
        translated_title: r.translated_title ?? null, progress: r.progress ?? null, ai_minutes: r.ai_minutes ?? null };
    },
    async minutesStatus(item_id) {
      const r = await call('memex.library.minutes.status', { item_id });
      return { state: r.state, reason: r.reason ?? null, text: r.text ?? null };
    },
    async minutesRequest(item_id) {
      const r = await call('memex.library.minutes.request', { item_id }, { write: true });
      return { state: r.state, reason: r.reason ?? null, text: r.text ?? null };
    },
    // PROPOSED (not final): server-paged translated Markdown, separate coordinate space from the original.
    async translationRead({ id, expectedRevision, startByte = 0 }) {
      const r = await call('memex.library.translation.read', { item_id: id, expected_revision_id: expectedRevision, start_byte: startByte });
      const { content_markdown: text, ...meta } = r;
      return { title: r.title, text, start_byte: r.start_byte, end_byte: r.end_byte, next_byte: r.next_byte ?? null,
        total_bytes: r.total_bytes, revision_id: r.revision_id, meta };
    },

    async updateItemMetadata({ item_id, expected_revision_id, title, manual_tags }) {
      const r = await call('memex.library.metadata.update', { item_id, expected_revision_id, title, manual_tags }, { write: true });
      return { previous_revision_id: r.previous_revision_id, revision_id: r.revision_id, revision_number: r.revision_number };
    },

    // Item chat on the Mac's chat route. A model answer can take up to two minutes; a timeout
    // is an unknown outcome (the Mac may still have called the model), never a silent resend.
    async itemChat({ id, requestId, messages }) {
      const r = await call('memex.library.item.chat', { item_id: id, request_id: requestId, messages }, { timeoutMs: 135_000 });
      return { reply: r.reply, model: r.model ?? null, finish: r.finish ?? 'stop' };
    },

    // AI insights of the current revision: {highlights, quotes, fact_check}, each
    // {state, reason, items, dropped}; items are null when the Mac could not fit them
    // (reason insights_too_large_for_read). A read: needs only the read grant.
    insightsStatus: (item_id) => call('memex.library.insights.status', { item_id }),
    // Explicit request (capture grant). The reply carries states only; the UI reads the items
    // back. A lost reply is `unknown` and is never resent.
    insightsRequest: (item_id, kinds) => call('memex.library.insights.request', { item_id, kinds }, { write: true }),

    // The item's conversation as stored on the Mac (every successful item.chat turn from any
    // client), oldest first. Pages are followed to the end (bounded); `partial` if cut short.
    async conversationRead(item_id) {
      const turns = [];
      let cursor = null; let version = null;
      for (let page = 0; page < 40; page += 1) {
        const r = await call('memex.library.conversation.read', { item_id, cursor });
        if (!Array.isArray(r.turns)) throw fail('invalid_response');
        version ??= r.version;
        turns.push(...r.turns.filter((t) => isObject(t) && ['user', 'assistant'].includes(t.role) && typeof t.content === 'string'));
        if (!r.next_cursor) return { turns, version, partial: false };
        cursor = r.next_cursor;
      }
      return { turns, version, partial: true };
    },

    // reader_v1 (only when session.info says reader: 'available'). toc → ReaderDocument: editions
    // with chapters {key,title,level,start,end,counterpart}; counterpart is declared chapter-order
    // correspondence only. chunk → one bounded piece of a chapter; offset is chapter-relative.
    async readerToc({ item_id, revision_id }) {
      const r = await call('memex.library.reader.toc', { item_id, revision_id });
      const chapterOk = (c) => isObject(c) && typeof c.key === 'string' && c.key && typeof c.title === 'string' && Number.isSafeInteger(c.level)
        && Number.isSafeInteger(c.start) && Number.isSafeInteger(c.end) && c.start <= c.end && (c.counterpart === null || c.counterpart === undefined || typeof c.counterpart === 'string');
      if (r.item_id !== item_id || r.revision_id !== revision_id || !Array.isArray(r.editions)
        || !r.editions.every((e) => isObject(e) && ['original', 'translation', 'source'].includes(e.side) && Array.isArray(e.chapters) && e.chapters.every(chapterOk))) throw fail('invalid_response');
      return { item_id, revision_id, canonical_sha256: r.canonical_sha256 ?? null, default_side: r.default_side ?? null, alignment: r.alignment ?? null,
        editions: r.editions.map((e) => ({ side: e.side, language: typeof e.language === 'string' ? e.language : '', sha256: e.sha256 ?? null, bytes: e.bytes ?? null,
          chapters: e.chapters.map((c) => ({ key: c.key, title: c.title, level: c.level, start: c.start, end: c.end, counterpart: c.counterpart ?? null })) })) };
    },
    // B2 cross-device reading state (reader_state_sync_v1; payloads are the UDS ones minus op).
    // state → {version, state}; save sends the complete next state with the version read (per-item CAS,
    // stale_reader_state on conflict; a lost reply is `unknown`, never resent with a new visit);
    // resume → [item_id, state] | null.
    async readerState({ item_id }) {
      const r = await call('memex.library.reader.state', { item_id });
      if (!isObject(r) || typeof r.version !== 'string' || !isObject(r.state)) throw fail('invalid_response');
      return { version: r.version, state: r.state };
    },
    readerSave: ({ item_id, expected_version, state }) => call('memex.library.reader.save', { item_id, expected_version, state }, { write: true }),
    readerResume: () => call('memex.library.reader.resume', {}),
    // Manual reader annotations v1 (reader_annotations_v1). list: one page bound to a ledger version
    // (null only on the first page); edit: one put/delete under CAS, never resent after a lost reply.
    // AI translation marks (reader_annotations_ai_translation_v1, only when session.info says
    // 'available'): the same methods with annotation_space "ai_translation" — a separate ledger whose
    // later pages also send the first page's view_version. Without annotation_space the params are
    // exactly the edition v1 wire (no AI key is ever sent).
    async readerAnnotationsList({ item_id, offset, limit, expected_version, annotation_space, projection, expected_view_version }) {
      const ai = annotation_space !== undefined;
      const params = ai ? { item_id, offset, limit, expected_version, annotation_space, ...(projection != null ? { projection } : {}), expected_view_version: expected_view_version ?? null }
        : { item_id, offset, limit, expected_version };
      const r = await call('memex.library.reader.annotations.list', params);
      if (!isObject(r) || r.item_id !== item_id || typeof r.version !== 'string' || !Array.isArray(r.annotations)
        || !(r.next_offset === null || Number.isSafeInteger(r.next_offset))) throw fail('invalid_response');
      if (ai && (r.annotation_space !== annotation_space || typeof r.view_version !== 'string'
        || !r.annotations.every((a) => isObject(a) && typeof a.id === 'string' && ['current', 'stale'].includes(a.status) && isObject(a.anchor)
          && a.anchor.side === 'ai_translation' && Array.isArray(a.anchor.segments) && a.anchor.segments.every((sg) => isObject(sg) && typeof sg.chapter === 'string'
            && typeof sg.translation_version === 'string' && typeof sg.sha256 === 'string' && Number.isSafeInteger(sg.start) && Number.isSafeInteger(sg.end) && sg.start < sg.end)))) throw fail('invalid_response');
      return r;
    },
    async readerAnnotationsEdit({ item_id, expected_version, change, annotation_space }) {
      const ai = annotation_space !== undefined;
      const r = await call('memex.library.reader.annotations.edit', ai ? { item_id, expected_version, annotation_space, change } : { item_id, expected_version, change }, { write: true });
      // A reply that does not name the AI ledger is not a confirmation: the outcome stays unknown.
      if (ai && (!isObject(r) || r.annotation_space !== annotation_space || typeof r.version !== 'string')) throw fail('unknown');
      return r;
    },
    // 阅读记录 (reader_passes_v1; only when session.info.reader_passes_v1 is 'available'). Params are
    // the UDS request minus op, exact keys. list / stats: one page bound to a version (null only at
    // offset 0). edit / timing_start / timing_report are writes: never resent here; a lost reply is
    // `unknown` (the app reads back, and resends the identical request only on a user's tap).
    async passesList({ item_id, offset, limit, expected_version }) {
      const r = await call('memex.library.reader.passes.list', { item_id, offset, limit, expected_version });
      if (!isObject(r) || r.item_id !== item_id || !hex64(r.version) || !Array.isArray(r.passes) || !count(r.total)
        || !(r.next_offset === null || count(r.next_offset)) || !r.passes.every((p) => isObject(p) && str(p.pass_id) && count(p.display_rank) && isObject(p.timing))) throw fail('invalid_response');
      return r;
    },
    async passesStats({ item_id, pass_id, offset, limit, expected_timing_version }) {
      const r = await call('memex.library.reader.passes.stats', { item_id, pass_id, offset, limit, expected_timing_version });
      if (!isObject(r) || r.item_id !== item_id || r.pass_id !== pass_id || !Array.isArray(r.days) || !(r.next_offset === null || count(r.next_offset))) throw fail('invalid_response');
      return r;
    },
    passesEdit: ({ item_id, expected_version, change }) => call('memex.library.reader.passes.edit', { item_id, expected_version, change }, { write: true }),
    passesTimingStart: ({ item_id, pass_id }) => call('memex.library.reader.passes.timing_start', { item_id, pass_id }, { write: true }),
    passesTimingReport: ({ item_id, pass_id, session_id, segments }) => call('memex.library.reader.passes.timing_report', { item_id, pass_id, session_id, segments }, { write: true }),
    async readerChunk({ item_id, revision_id, side, chapter, offset, limit }, { background = false } = {}) {
      const r = await call('memex.library.reader.chunk', { item_id, revision_id, side, chapter, offset, limit }, { background });
      if (r.item_id !== item_id || r.revision_id !== revision_id || r.side !== side || r.chapter !== chapter || typeof r.markdown !== 'string'
        || !Number.isSafeInteger(r.start) || !Number.isSafeInteger(r.end) || r.end < r.start
        || new TextEncoder().encode(r.markdown).length !== r.end - r.start
        || !(r.next_offset === null || r.next_offset === undefined || (Number.isSafeInteger(r.next_offset) && r.next_offset > offset))) throw fail('invalid_response');
      return { item_id, revision_id, side, sha256: r.sha256 ?? null, evidence_id: r.evidence_id ?? null, chapter, start: r.start, end: r.end,
        next_offset: r.next_offset ?? null, markdown: r.markdown };
    },

    // Paragraph correspondence of one ORIGINAL chapter (frozen reader_v1 seam; only when
    // session.info.reader_alignment is 'available'). One page of pair rows; the app follows
    // next_offset with the returned version and checks the whole partition and both chapter hashes.
    async readerAlignment({ item_id, revision_id, chapter, offset, limit, expected_version }) {
      const r = await call('memex.library.reader.alignment', { item_id, revision_id, chapter, offset, limit, expected_version });
      const hex = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
      const range = (v) => v === null || (Array.isArray(v) && v.length === 2 && v.every(Number.isSafeInteger) && v[0] >= 0 && v[0] < v[1]);
      const pairOk = (p) => isObject(p) && range(p.o) && range(p.t) && ['paragraph', 'heading', 'note'].includes(p.kind)
        && ['matched', 'original_only', 'translation_only', 'unresolved'].includes(p.relation);
      const tNull = r.translation_chapter === null;
      if (r.schema_version !== 1 || r.item_id !== item_id || r.revision_id !== revision_id || r.original_chapter !== chapter
        || !(r.version === 'missing' || hex(r.version)) || !(expected_version === 'any' || r.version === expected_version)
        || !(tNull || (typeof r.translation_chapter === 'string' && r.translation_chapter))
        || !hex(r.original_sha256) || !Number.isSafeInteger(r.original_bytes) || r.original_bytes < 0
        || (tNull ? r.translation_sha256 !== null || r.translation_bytes !== null : !hex(r.translation_sha256) || !Number.isSafeInteger(r.translation_bytes) || r.translation_bytes < 0)
        || !['aligned', 'partial', 'none'].includes(r.status) || r.offset !== offset || !Number.isSafeInteger(r.total) || r.total < 0
        || !Array.isArray(r.pairs) || r.pairs.length > limit || !r.pairs.every(pairOk)
        || r.next_offset !== (offset + r.pairs.length < r.total ? offset + r.pairs.length : null)
        || (r.next_offset === null && offset + r.pairs.length !== r.total)) throw fail('invalid_response');
      return { version: r.version, original_chapter: r.original_chapter, translation_chapter: r.translation_chapter,
        original_sha256: r.original_sha256, translation_sha256: r.translation_sha256, original_bytes: r.original_bytes, translation_bytes: r.translation_bytes,
        status: r.status, quality: r.quality ?? null, offset: r.offset, total: r.total, next_offset: r.next_offset,
        pairs: r.pairs.map((p) => ({ o: p.o && [p.o[0], p.o[1]], t: p.t && [p.t[0], p.t[1]], kind: p.kind, relation: p.relation })) };
    },
    // Explicit chapter repair (edit grant): the request_id is persisted by the caller before this is
    // sent. A lost or malformed reply is `unknown` (the job may exist): resend the SAME id and payload.
    async readerAlignRequest({ item_id, revision_id, chapter, request_id, expected_alignment_version }) {
      const r = await call('memex.library.reader.align_request', { item_id, revision_id, chapter, request_id, expected_alignment_version }, { write: true });
      const job = alignJob(r);
      if (!job || job.request_id !== request_id || job.item_id !== item_id || job.revision_id !== revision_id || job.chapter !== chapter) throw fail('unknown');
      return job;
    },
    async readerAlignStatus({ item_id, job_id }) {
      const job = alignJob(await call('memex.library.reader.align_status', { item_id, job_id }));
      if (!job || job.job_id !== job_id || job.item_id !== item_id) throw fail('invalid_response');
      return job;
    },

    // 翻译本章 (only when session.info.reader_translation is 'available'). preview is a read: no
    // inference, no write; it returns the saved Translation route and a bounded quote, or the
    // completed AI projection (then quote is null and nothing is spent).
    async readerTranslatePreview({ item_id, revision_id, chapter }) {
      const r = await call('memex.library.reader.translate_preview', { item_id, revision_id, chapter });
      if (typeof r.eligible !== 'boolean' || !strOrNull(r.reason ?? null)) throw fail('invalid_response');
      const translation = r.translation == null ? null : xlProjection(r.translation);
      if (r.translation != null && (!translation || translation.item_id !== item_id || translation.original_chapter !== chapter)) throw fail('invalid_response');
      let quote = null;
      if (r.quote != null) {
        const q = r.quote; const route = isObject(q) ? xlRoute(q.route) : null; const budget = isObject(q) ? xlBudget(q.budget) : null;
        const cost = q?.estimated_cost;
        if (!route || !budget || !/^quote_[a-f0-9]{32}$/.test(q.quote_id || '') || !count(q.expires_at) || q.item_id !== item_id || q.revision_id !== revision_id
          || q.chapter !== chapter || !hex64(q.source_sha256) || !Number.isSafeInteger(q.planned_requests) || q.planned_requests < 1 || q.planned_requests > 8
          || budget.max_requests !== q.planned_requests || !count(q.source_bytes)
          || !(cost == null || (isObject(cost) && str(cost.amount) && /^[A-Z]{3}$/.test(cost.currency || '') && cost.kind === 'estimate'))) throw fail('invalid_response');
        quote = { quote_id: q.quote_id, expires_at: q.expires_at, item_id, revision_id, chapter, source_sha256: q.source_sha256, target_language: q.target_language ?? null,
          route, budget, source_bytes: q.source_bytes, planned_requests: q.planned_requests, estimated_cost: cost ? { amount: cost.amount, currency: cost.currency, kind: cost.kind } : null };
      }
      return { eligible: r.eligible, reason: r.reason ?? null, quote, translation };
    },
    // Explicit, confirmed translation (edit grant). The caller persists request_id + quote_id before
    // sending; a lost or malformed reply is `unknown` and is only ever resolved by the SAME payload.
    async readerTranslateRequest({ item_id, revision_id, chapter, request_id, quote_id, accept_cost, expected_translation_version }) {
      const r = await call('memex.library.reader.translate_request', { item_id, revision_id, chapter, request_id, quote_id, accept_cost, expected_translation_version }, { write: true });
      const job = xlJob(r);
      if (!job || job.request_id !== request_id || job.item_id !== item_id || job.chapter !== chapter) throw fail('unknown');
      return job;
    },
    async readerTranslateStatus({ item_id, job_id }) {
      const job = xlJob(await call('memex.library.reader.translate_status', { item_id, job_id }));
      if (!job || job.job_id !== job_id || job.item_id !== item_id) throw fail('invalid_response');
      return job;
    },
    // One bounded piece of the generated text; offsets are relative to the generated text.
    async readerTranslateChunk({ item_id, revision_id, chapter, expected_version, offset, limit }) {
      const r = await call('memex.library.reader.translate_chunk', { item_id, revision_id, chapter, expected_version, offset, limit });
      const meta = xlProjection(r);
      if (!meta || meta.item_id !== item_id || meta.revision_id !== revision_id || meta.original_chapter !== chapter || meta.version !== expected_version
        || typeof r.markdown !== 'string' || r.start !== offset || !Number.isSafeInteger(r.end) || r.end < r.start || r.end > meta.bytes
        || new TextEncoder().encode(r.markdown).length !== r.end - r.start
        || !(r.next_offset == null ? r.end === meta.bytes : r.next_offset === r.end && r.end > offset)) throw fail('invalid_response');
      return { ...meta, start: r.start, end: r.end, next_offset: r.next_offset ?? null, markdown: r.markdown };
    },
    // Credential-free discovery of a completed AI translation of this exact chapter (only when
    // session.info.reader_translation_metadata is 'available'): null or the projection metadata.
    // No quote, no job, no body text; null is not proof the chapter can never have one.
    async readerTranslateMetadata({ item_id, revision_id, chapter }) {
      const r = await call('memex.library.reader.translate_metadata', { item_id, revision_id, chapter });
      if (r.item_id !== item_id || r.revision_id !== revision_id || r.chapter !== chapter || r.translation === undefined) throw fail('invalid_response');
      const translation = r.translation == null ? null : xlProjection(r.translation);
      if (r.translation != null && (!translation || translation.item_id !== item_id || translation.revision_id !== revision_id || translation.original_chapter !== chapter)) throw fail('invalid_response');
      return { translation };
    },
    // Recorded source-to-AI-output block correspondence of that projection, one page (only when
    // session.info.reader_translation_alignment is 'available'). Rows are {o,t,kind,relation:matched};
    // o is relative to the original chapter, t to the AI body. status=missing for a legacy projection.
    async readerTranslateAlignment({ item_id, revision_id, chapter, expected_version, offset, limit }) {
      const r = await call('memex.library.reader.translate_alignment', { item_id, revision_id, chapter, expected_version, offset, limit });
      const range = (v) => Array.isArray(v) && v.length === 2 && v.every(count) && v[0] < v[1];
      const pairs = Array.isArray(r.pairs) ? r.pairs : null;
      if (r.item_id !== item_id || r.revision_id !== revision_id || r.version !== expected_version || r.original_chapter !== chapter || r.translation_chapter !== null
        || !hex64(r.original_sha256) || !hex64(r.translation_sha256) || !count(r.original_bytes) || !count(r.translation_bytes)
        || !['aligned', 'missing'].includes(r.status) || r.quality !== 'not_certified' || r.basis !== 'translation_blocks'
        || r.offset !== offset || !count(r.total) || !pairs || pairs.length > limit
        || !pairs.every((p) => isObject(p) && range(p.o) && range(p.t) && p.relation === 'matched' && ['paragraph', 'heading', 'note'].includes(p.kind))
        || r.next_offset !== (offset + pairs.length < r.total ? offset + pairs.length : null)
        || (r.status === 'missing' && (r.total !== 0 || pairs.length !== 0))) throw fail('invalid_response');
      return { version: r.version, original_sha256: r.original_sha256, translation_sha256: r.translation_sha256, original_bytes: r.original_bytes, translation_bytes: r.translation_bytes,
        status: r.status, offset: r.offset, total: r.total, next_offset: r.next_offset, pairs: pairs.map((p) => ({ o: [...p.o], t: [...p.t], kind: p.kind, relation: 'matched' })) };
    },

    // 编辑 Markdown of one chapter of one side (only when session.info.reader_edit is 'available' and
    // the pairing has the edit grant). Staged: begin → chunks (≤8 KiB, whole code points) → commit.
    // The gateway binds the stage to this session's principal; the client never sends one. Any
    // malformed reply is `unknown` (the step may have been applied): resolved only by replaying the
    // SAME request id, which resumes the stage or returns the committed receipt.
    async readerEditBegin({ item_id, revision_id, side, chapter, expected_chapter_sha256, request_id, bytes, sha256 }) {
      const r = await call('memex.library.reader.edit_begin', { item_id, revision_id, side, chapter, expected_chapter_sha256, request_id, bytes, sha256 }, { write: true });
      const receipt = r.receipt == null ? null : editReceipt(r.receipt);
      if (!/^edit_[a-f0-9]{64}$/.test(r.edit_id || '') || !count(r.received_bytes) || r.received_bytes > bytes || !Number.isSafeInteger(r.max_chunk_bytes) || r.max_chunk_bytes < 4
        || !['uploading', 'committed'].includes(r.state) || (r.receipt != null && !receipt) || (r.state === 'committed') !== !!receipt) throw fail('unknown');
      return { edit_id: r.edit_id, received_bytes: r.received_bytes, max_chunk_bytes: r.max_chunk_bytes, state: r.state, receipt };
    },
    async readerEditChunk({ item_id, edit_id, offset, markdown }) {
      const r = await call('memex.library.reader.edit_chunk', { item_id, edit_id, offset, markdown }, { write: true });
      if (r.edit_id !== edit_id || !count(r.received_bytes)) throw fail('unknown');
      return { edit_id: r.edit_id, received_bytes: r.received_bytes };
    },
    async readerEditCommit({ item_id, edit_id }) {
      const receipt = editReceipt(await call('memex.library.reader.edit_commit', { item_id, edit_id }, { write: true }));
      if (!receipt) throw fail('unknown');
      return receipt;
    },
    // 对比修改 (only when session.info.reader_revision_text is 'available'): exact historical text of
    // one target ({kind:'body'} or {kind:'chapter',side,chapter}) of a retained revision, by bounded
    // pieces. The caller follows next_offset with the first piece's sha256 and checks the whole text.
    async readerRevisionChunk({ item_id, revision_id, target, start_byte, limit_bytes, expected_sha256 }) {
      const r = await call('memex.library.reader.revision_chunk', { item_id, revision_id, target, start_byte, limit_bytes, expected_sha256 });
      const t = r.target;
      const sameTarget = isObject(t) && t.kind === target.kind && (target.kind === 'body' || (t.side === target.side && t.chapter === target.chapter));
      if (r.item_id !== item_id || r.revision_id !== revision_id || !sameTarget || !hex64(r.sha256) || (expected_sha256 !== 'any' && r.sha256 !== expected_sha256)
        || !count(r.total_bytes) || r.start_byte !== start_byte || typeof r.text !== 'string' || !count(r.end_byte)
        || new TextEncoder().encode(r.text).length !== r.end_byte - start_byte || r.end_byte - start_byte > limit_bytes || r.end_byte > r.total_bytes
        || r.next_offset !== (r.end_byte < r.total_bytes ? r.end_byte : null) || (r.end_byte < r.total_bytes && r.end_byte === start_byte)) throw fail('invalid_response');
      return { sha256: r.sha256, total_bytes: r.total_bytes, start_byte: r.start_byte, end_byte: r.end_byte, next_offset: r.next_offset, text: r.text };
    },

    // 格式整理 (reader_format / reader_format_batch). The gateway injects the principal. preview /
    // targets / status / results / changes are reads; request / restore / resume are edit-scope
    // writes whose lost or malformed reply is `unknown` (resolved only by the SAME request).
    async readerFormatPreview(params) {
      const r = await call('memex.library.reader.format_preview', params);
      const q = r.quote;
      if (!str(r.preview_id) || !['preparing', 'ready', 'failed'].includes(r.state) || (r.state === 'ready') !== (q != null) || (q != null && !fmtQuote(q))) throw fail('invalid_response');
      return { preview_id: r.preview_id, state: r.state, quote: q == null ? null : fmtQuote(q), error_code: r.error_code ?? null };
    },
    async readerFormatTargets({ preview_id, offset, limit }) {
      const r = await call('memex.library.reader.format_targets', { preview_id, offset, limit });
      if (r.preview_id !== preview_id || !count(r.total) || r.offset !== offset || !Array.isArray(r.targets) || r.targets.length > limit
        || !r.targets.every((t) => isObject(t) && str(t.target_id) && str(t.item_id) && ['eligible', 'skipped'].includes(t.state))) throw fail('invalid_response');
      return { total: r.total, offset: r.offset, next_offset: r.next_offset ?? null, targets: r.targets.map((t) => ({ target_id: t.target_id, item_id: t.item_id, revision_id: t.revision_id ?? null,
        target: fmtTarget(t.target), state: t.state, reason: t.reason ?? null, source_bytes: countOrNull(t.source_bytes), budget: t.budget == null ? null : xlBudget(t.budget) })) };
    },
    async readerFormatRequest({ quote_id, request_id }) {
      const r = await call('memex.library.reader.format_request', { quote_id, request_id, accept_cost: true, accept_model_processing: true }, { write: true });
      const batch = fmtBatch(r.batch);
      if (!batch || batch.request_id !== request_id) throw fail('unknown');
      return batch;
    },
    async readerFormatStatus({ batch_id }) {
      const batch = fmtBatch((await call('memex.library.reader.format_status', { batch_id })).batch);
      if (!batch || batch.batch_id !== batch_id) throw fail('invalid_response');
      return batch;
    },
    async readerFormatResults({ batch_id, offset, limit }) {
      const r = await call('memex.library.reader.format_results', { batch_id, offset, limit });
      const rows = Array.isArray(r.results) ? r.results.map(fmtRow) : null;
      if (r.batch_id !== batch_id || !count(r.total) || r.offset !== offset || !rows || rows.some((x) => !x) || rows.length > limit
        || r.next_offset !== (offset + rows.length < r.total ? offset + rows.length : null)) throw fail('invalid_response');
      return { total: r.total, offset, next_offset: r.next_offset, results: rows };
    },
    async readerFormatChanges(params) {
      const r = await call('memex.library.reader.format_changes', params);
      if (r.version !== params.expected_version) throw fail('invalid_response');
      if (params.view === 'text') {
        if (r.change_id !== params.change_id || r.part !== params.part || typeof r.text !== 'string' || r.start_byte !== params.start_byte || !count(r.total_bytes)
          || new TextEncoder().encode(r.text).length !== r.end_byte - r.start_byte || r.end_byte > r.total_bytes
          || r.next_offset !== (r.end_byte < r.total_bytes ? r.end_byte : null)) throw fail('invalid_response');
        return { total_bytes: r.total_bytes, start_byte: r.start_byte, end_byte: r.end_byte, next_offset: r.next_offset, text: r.text };
      }
      const range = (v) => Array.isArray(v) && v.length === 2 && v.every(count) && v[0] <= v[1];
      const ok = (c) => isObject(c) && str(c.id) && str(c.kind) && range(c.old_range) && range(c.new_range) && count(c.before_bytes) && count(c.after_bytes);
      if (!str(r.item_id) || !str(r.input_revision_id) || !str(r.result_revision_id) || !hex64(r.source_sha256) || !hex64(r.result_sha256) || !count(r.total) || r.offset !== params.offset
        || !Array.isArray(r.changes) || !r.changes.every(ok) || r.next_offset !== (params.offset + r.changes.length < r.total ? params.offset + r.changes.length : null)) throw fail('invalid_response');
      return { item_id: r.item_id, input_revision_id: r.input_revision_id, result_revision_id: r.result_revision_id, target: fmtTarget(r.target), source_sha256: r.source_sha256, result_sha256: r.result_sha256,
        total: r.total, offset: r.offset, next_offset: r.next_offset, changes: r.changes.map((c) => ({ id: c.id, kind: c.kind, basis: c.basis ?? null, reason_code: c.reason_code ?? null,
          old_range: c.old_range, new_range: c.new_range, before_bytes: c.before_bytes, after_bytes: c.after_bytes })) };
    },
    async readerFormatRestore({ batch_id, target_id, expected_revision_id, request_id }) {
      const r = await call('memex.library.reader.format_restore', { batch_id, target_id, expected_revision_id, request_id }, { write: true });
      if (!str(r.item_id) || r.request_id !== request_id || !str(r.revision_id) || typeof r.changed !== 'boolean') throw fail('unknown');
      return { item_id: r.item_id, request_id: r.request_id, revision_id: r.revision_id, changed: r.changed };
    },
    async readerFormatResume({ batch_id }) {
      const batch = fmtBatch((await call('memex.library.reader.format_resume', { batch_id }, { write: true })).batch);
      if (!batch || batch.batch_id !== batch_id) throw fail('unknown');
      return batch;
    },

    // scopes / max_sensitivity / expires_at_ms / generation_ready / chat / translation /
    // minutes / insights / conversation
    sessionInfo: () => call('memex.session.info', {}),
    // memex.library.groups.tree: the authorized group tree, preorder, paged and bound to `view`
    // (stale_group_view → restart). A gateway without it answers invalid_request.
    groupsTree: ({ offset = 0, limit = 500, expected_view = null }) => call('memex.library.groups.tree', { offset, limit, expected_view }),
    reorderGroup: (params) => call('memex.library.groups.reorder', params, { write: true }),
    // 当前分组 (ui_active_group_v1; the app calls it only when session.info.ui_active_group_v1 is
    // 'available' and the pairing has the edit grant): the library root or folder the user explicitly
    // opened (null for a virtual view); the Chrome extension files into it when 收进 Inbox is off.
    // A write: a lost or malformed reply is `unknown`, resolved only by resending the SAME request_id.
    // interfaces/memex.chrome_capture_v1.md (Active UI destination): params exactly {request_id
    // ([A-Za-z0-9_-]{16,64}), group_id (string | null)}; the reply is the UDS acknowledgement
    // {schema_version: 1, request_id, group_id, updated_at}, whose request_id is the gateway's
    // principal-namespaced id (not ours), so only its shape is checked; group_id must echo ours.
    async uiActiveGroupSet({ request_id, group_id }) {
      const r = await call('memex.library.ui.active_group.set', { request_id, group_id }, { write: true });
      if (!exactKeys(r, ['schema_version', 'request_id', 'group_id', 'updated_at']) || r.schema_version !== 1 || !str(r.request_id)
        || r.group_id !== group_id || !str(r.updated_at)) throw fail('unknown');
      return { schema_version: 1, request_id: r.request_id, group_id: r.group_id, updated_at: r.updated_at };
    },
    groupTrash: (group_id, cursor = null) => call('memex.library.trash.list', { group_id, cursor }),
    editGroupTrash: (params) => call('memex.library.trash.edit', params, { write: true }),
  };
}

// ---------- connection (gateway mode only) ----------
// Wires the existing /remote/v1 pairing and encryption modules together; no crypto or protocol of
// its own. Returned object is the `channel` for createGatewaySource.
const notSent = () => Object.assign(new Error('unavailable'), { code: 'unavailable' });

export async function openConnection({ onChange = () => {} } = {}) {
  const [app, protocol, storage, scanner, config] = await Promise.all([
    import('/remote/v1/app.mjs'),
    import('/remote/v1/protocol.mjs'),
    import('/remote/v1/storage.mjs'),
    import('/remote/v1/scanner.mjs'),
    import('/remote/v1/config.mjs'),
  ]);
  const cfg = { webClientUrl: config.WEB_CLIENT_URL, relayUrl: config.RELAY_URL, allowLocal: config.ALLOW_LOCAL };
  // The pairing protocol validates the exact page URL (no query or fragment).
  const pageHref = location.href;

  await storage.acquireBrowserLease();
  await storage.runCapabilityProbe();
  const db = await storage.openDatabase();
  const identity = await storage.loadOrCreateIdentity(db);
  const phoneId = await storage.loadOrCreatePhoneId(db);

  let pairing = null;
  const stored = await storage.getState(db, 'pairing');
  if (stored) {
    try {
      const p = protocol.validatePersistedPairing(stored, pageHref, cfg);
      if (p.trusted) pairing = p;
    } catch { /* unusable record: the user pairs again */ }
  }

  let channel = null;
  let opening = null;
  // The channel closed after being open (not by us) and is not back yet; closeCode is the relay's
  // reason: 4002 mac_offline / peer_offline and 4005 mean the Mac leg is gone.
  let down = false; let closeCode = null;
  // A channel whose socket is no longer open is gone, even before its close event reached us (the
  // secure session rejects pending requests first, and a replay may run in between).
  const live = () => (channel && channel.socket?.readyState === 1 ? channel : null);
  function ensureChannel() {
    if (live()) return Promise.resolve(channel);
    if (!pairing) return Promise.reject(notSent());
    opening ??= app.openSecureChannel({
      pairing, identity, phoneId, allowLocal: cfg.allowLocal,
      persistTrust: async (trusted) => { await storage.putState(db, 'pairing', trusted); pairing = trusted; },
    }).then((ch) => {
      channel = ch; down = false; closeCode = null;
      ch.socket.addEventListener('close', (ev) => {
        if (channel !== ch) return;
        channel = null; down = true; closeCode = Number.isInteger(ev?.code) ? ev.code : null;
        // Come back by ourselves (not when another page of this phone replaced us: 4003).
        if (closeCode !== 4003) scheduleRetry(1000);
        onChange();
      });
      onChange();
      return ch;
    }, () => { scheduleRetry(); throw notSent(); }).finally(() => { opening = null; });
    return opening;
  }
  // While paired and offline, keep trying in the background (the Mac bridge may be reconnecting);
  // onChange fires when the channel comes back so the UI can reload.
  let retryTimer = null;
  function scheduleRetry(ms = 3000) {
    if (retryTimer || !pairing?.trusted) return;
    retryTimer = setTimeout(() => { retryTimer = null; ensureChannel().catch(() => {}); }, ms);
  }

  return {
    get paired() { return !!pairing; },
    get connected() { return !!channel; },
    // The link dropped and is not back yet (the header must not say 已连接 meanwhile).
    get reconnecting() { return !channel && down && !!pairing?.trusted; },
    // Waits up to `ms` for an open channel (re-opening it as needed). false when unpaired or out of time.
    async waitReady(ms) {
      const end = Date.now() + Math.max(0, ms);
      for (;;) {
        if (!pairing?.trusted) return false;
        if (live()) return true;
        const left = end - Date.now();
        if (left <= 0) return false;
        let timer;
        const ok = await Promise.race([ensureChannel().then(() => true, () => false), new Promise((r) => { timer = setTimeout(() => r(false), left); })]);
        clearTimeout(timer);
        if (ok) return true;
        const rest = end - Date.now();
        if (rest <= 0) return false;
        await new Promise((r) => setTimeout(r, Math.min(1500, rest)));
      }
    },
    // Identifies the current trusted pairing (stable across reconnects, new after re-pairing);
    // the article cache is bound to it. Not a secret, but only its hash is stored.
    get scope() {
      return pairing?.trusted ? `${pairing.mac_device_id}|${pairing.session_id}|${pairing.pairing_generation}` : null;
    },

    // Pairing code from the in-page scanner or pasted text; never from the URL.
    async pair(code) {
      const candidate = protocol.parsePairingCode(String(code).trim(), pageHref, cfg);
      pairing = candidate;
      try { await ensureChannel(); } catch (e) { if (!pairing.trusted) pairing = null; throw e; }
      onChange();
    },
    async scan({ video, canvas, signal }) {
      if (!globalThis.jsQR) {
        await new Promise((resolve, reject) => {
          const s = Object.assign(document.createElement('script'), { src: '/remote/v1/jsqr.js', onload: resolve, onerror: reject });
          document.head.append(s);
        });
      }
      return scanner.scanPairingCode({ video, canvas, signal });
    },

    // Same shape as openSecureChannel's request. A request that could not be sent rejects with
    // { code: 'unavailable' }; any other rejection happened after sending.
    async request(id, method, params, options) {
      const ch = await ensureChannel();
      return ch.request(id, method, params, options);
    },
    connect: () => ensureChannel().catch(() => {}),
    // Forget this Mac's trust on this page so the user can pair again. Jobs from the old pairing
    // stay owned by it on the Mac and are no longer readable from here.
    async unpair() {
      const ch = channel; channel = null; pairing = null; down = false;
      try { ch?.close(); } catch { /* already closed */ }
      await storage.deleteState(db, 'pairing');
      onChange();
    },
  };
}
