'use strict';

const { MAX_PLAINTEXT_BYTES, assertExactKeys } = require('../protocol');
const { sendOwnerCommand } = require('./ipc-client');
const { createHash } = require('node:crypto');
const fs = require('node:fs');

// Short digest of this router's own source: the gateway build the phone talks to.
const ROUTER_SHA256 = createHash('sha256').update(fs.readFileSync(__filename)).digest('hex').slice(0, 12);

const LEVELS = ['public', 'internal', 'private', 'restricted'];
const METHODS = new Set([
  'memex.session.info', 'memex.library.query', 'memex.library.item.get',
  'memex.library.context.read', 'memex.library.metadata.update',
  'memex.capture.enqueue', 'memex.capture.status',
  'memex.library.translation.status', 'memex.library.translation.read',
  'memex.library.item.chat', 'memex.library.minutes.status', 'memex.library.minutes.request',
  'memex.library.translation.request', 'memex.library.insights.status',
  'memex.library.insights.request', 'memex.library.conversation.read', 'memex.library.groups.tree',
  'memex.library.groups.reorder',
  'memex.library.ui.active_group.set',
  ...['delete_preview', 'delete', 'delete_list', 'delete_restore'].map((op) => `memex.library.groups.${op}`),
  'memex.library.trash.list', 'memex.library.trash.edit',
  'memex.library.image.read',
  'memex.library.item_cover_batch', 'memex.library.item_cover_chunk',
  'memex.library.reader.toc', 'memex.library.reader.chunk',
  'memex.library.reader.alignment',
  'memex.library.reader.align_request', 'memex.library.reader.align_status',
  'memex.library.reader.edit_begin', 'memex.library.reader.edit_chunk', 'memex.library.reader.edit_commit',
  'memex.library.reader.translate_preview', 'memex.library.reader.translate_request',
  'memex.library.reader.translate_status', 'memex.library.reader.translate_chunk',
  'memex.library.reader.translate_metadata', 'memex.library.reader.translate_alignment',
  'memex.library.reader.revision_chunk',
  'memex.library.reader.state', 'memex.library.reader.save', 'memex.library.reader.resume',
  'memex.library.reader.annotations.list', 'memex.library.reader.annotations.edit',
  ...['list', 'edit', 'stats', 'timing_start', 'timing_report'].map((op) => `memex.library.reader.passes.${op}`),
  ...['preview', 'start', 'status', 'stop'].map((op) => `memex.library.reader.align_batch_${op}`),
  ...['preview', 'request', 'status', 'stop', 'read'].map((op) => `memex.library.reader.translate_map_${op}`),
  ...['preview', 'targets', 'request', 'status', 'results', 'changes', 'restore', 'resume']
    .map((op) => `memex.library.reader.format_${op}`),
]);
// Explicit requests that start durable provider work on the Mac (same nature as a capture job).
const REQUEST_COMMANDS = new Set(['minutes_request', 'translation_request', 'insight_request']);
const INSIGHT_KINDS = ['highlights', 'quotes', 'fact_check'];
const QUERY_SORTS = ['title', 'source', 'captured_at', 'added_at', 'item_type'];
const CHAT_TIMEOUT_MS = 130_000;
const MAX_REQUEST_BYTES = 96 * 1024;
const ERRORS = new Set([
  'invalid_cover_request', 'invalid_cover_offset', 'stale_cover_version', 'cover_unavailable',
  'image_resource_not_found', 'image_read_failed', 'image_too_large', 'invalid_image_range',
  'invalid_request', 'internal_error', 'permission_denied', 'session_expired', 'session_revoked',
  'remote_resource_not_found', 'request_identity_conflict', 'translation_unavailable',
  'library_resource_not_found', 'library_read_failed', 'library_query_failed', 'library_loading',
  'invalid_query_window', 'stale_query_view', 'stale_item_revision',
  'invalid_reader_request', 'invalid_reader_offset', 'invalid_reader_limit',
  'invalid_reader_state', 'invalid_reader_anchor', 'stale_reader_state',
  'invalid_reader_annotation', 'stale_reader_annotations', 'reader_annotation_conflict',
  'reader_annotation_limit_exceeded', 'reader_annotation_not_found', 'reader_annotations_unavailable',
  'invalid_reader_pass', 'invalid_reader_pass_dates', 'invalid_reader_pass_timing', 'stale_reader_passes', 'stale_reader_pass_timing', 'reader_pass_conflict', 'reader_pass_not_found', 'reader_pass_limit_exceeded', 'reader_pass_closed', 'reader_pass_not_timeable', 'reader_pass_session_expired', 'reader_pass_clock_discontinuity', 'reader_pass_segment_conflict', 'reader_passes_unavailable',
  'revision_text_unavailable',
  'invalid_content_format_request', 'content_format_unavailable', 'content_format_preview_not_found',
  'content_format_quote_stale', 'content_format_quote_consumed', 'content_format_batch_not_found',
  'content_format_changes_stale', 'content_format_restore_conflict', 'content_format_invalid_result',
  'invalid_reader_alignment_request', 'stale_chapter_alignment',
  'reader_job_in_progress', 'reader_job_not_found', 'no_low_dial_provider',
  'reader_repair_budget_exceeded', 'reader_repair_failed',
  'invalid_reader_edit', 'reader_edit_not_found', 'reader_edit_too_large', 'reader_edit_upload_conflict',
  'invalid_reader_translation_request', 'reader_translation_not_needed',
  'reader_translation_language_unsupported', 'reader_translation_budget_exceeded',
  'reader_translation_quote_stale', 'reader_translation_unavailable', 'stale_reader_translation', 'no_translation_provider',
  'reader_side_not_found', 'reader_chapter_not_found', 'stale_book_manifest',
  'invalid_context_request', 'invalid_context_range', 'context_range_too_large',
  'context_resource_not_found', 'context_read_failed', 'invalid_metadata_update',
  'metadata_unchanged', 'item_revision_limit_exceeded', 'result_too_large',
  'daemon_unavailable', 'no_chat_provider', 'provider_outcome_unknown', 'provider_failure',
  'invalid_chat_request', 'no_minutes_provider', 'no_insight_provider', 'invalid_insight_request',
  'invalid_conversation_request', 'invalid_group_window', 'stale_group_view',
  'invalid_group_edit', 'stale_group_version', 'groups_unchanged', 'request_id_conflict',
  'group_limit_exceeded', 'group_request_limit_exceeded',
  'invalid_trash_request', 'item_already_trashed', 'item_not_trashed', 'invalid_cursor',
]);

// Ephemeral, Mac-owned grants for the isolated gateway. Pairing establishes
// identity, not permissions. No inherited owner access or persistent ACL edit.
class SessionGrants {
  constructor({ now = () => Date.now() } = {}) {
    this.now = now;
    this.grants = new Map();
    this.issued = new WeakSet();
  }

  set({ sessionId, generation, scopes, maxSensitivity, expiresAt }) {
    if (typeof sessionId !== 'string' || !Number.isSafeInteger(generation)
      || !Array.isArray(scopes) || !scopes.includes('read')
      || scopes.some((scope) => !['read', 'edit', 'capture'].includes(scope))
      || !LEVELS.includes(maxSensitivity) || !Number.isSafeInteger(expiresAt)
      || expiresAt <= this.now()) throw new Error('invalid_grant');
    const grant = Object.freeze({
      sessionId, generation, scopes: Object.freeze([...new Set(scopes)]),
      maxSensitivity, expiresAt,
    });
    this.grants.set(sessionId, grant);
    this.issued.add(grant);
    return grant;
  }

  revoke(sessionId) { this.grants.delete(sessionId); }

  snapshot(channel) {
    const grant = this.grants.get(channel.sessionId);
    this.assertCurrent(channel, grant);
    return grant;
  }

  assertCurrent(channel, grant) {
    if (!grant || !this.issued.has(grant) || this.grants.get(channel.sessionId) !== grant
      || grant.generation !== channel.generation) throw new Error('session_revoked');
    if (grant.expiresAt <= this.now()) throw new Error('session_expired');
  }
}

function commandFor(request, grant, principal) {
  const p = request.params;
  const max_sensitivity = grant.maxSensitivity;
  // These durable workers require live authority after this RPC returns.
  // Until that authenticated seam exists, even an edit grant cannot admit them.
  if (request.method.startsWith('memex.library.reader.align_batch_')
      || request.method.startsWith('memex.library.reader.translate_map_')
      || request.method.startsWith('memex.library.groups.delete')) {
    throw new Error('permission_denied');
  }
  switch (request.method) {
    case 'memex.library.ui.active_group.set': {
      if (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) throw new Error('permission_denied');
      assertExactKeys(p, ['request_id', 'group_id'], 'invalid_request');
      if (typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{16,64}$/.test(p.request_id)
        || !(p.group_id === null || (typeof p.group_id === 'string' && /^group_[A-Za-z0-9_]+$/.test(p.group_id) && p.group_id.length <= 128))) throw new Error('invalid_request');
      return { command: 'ui_active_group_set', group_id: p.group_id, max_sensitivity,
        request_id: `mobile-ui-${createHash('sha256').update(`${principal}:${p.request_id}`).digest('hex')}` };
    }
    case 'memex.library.reader.format_preview':
    case 'memex.library.reader.format_targets':
    case 'memex.library.reader.format_request':
    case 'memex.library.reader.format_status':
    case 'memex.library.reader.format_results':
    case 'memex.library.reader.format_changes':
    case 'memex.library.reader.format_restore':
    case 'memex.library.reader.format_resume': {
      const error = 'invalid_content_format_request';
      const op = request.method.split('.').pop();
      if (['format_request', 'format_restore', 'format_resume'].includes(op)
        && !grant.scopes.includes('edit')) throw new Error('permission_denied');
      // Only the authenticated channel supplies this identity, never request params.
      if (typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) throw new Error('permission_denied');
      const id = (v) => typeof v === 'string' && /^[A-Za-z0-9_-]{1,256}$/.test(v);
      const page = (offset, limit, max) => Number.isSafeInteger(offset) && offset >= 0
        && Number.isInteger(limit) && limit >= 1 && limit <= max;
      let fields;
      if (op === 'format_preview') {
        fields = p?.selection !== undefined ? ['selection'] : ['preview_id'];
      } else if (op === 'format_changes') {
        fields = ['batch_id', 'target_id', 'expected_version', 'view',
          ...(p?.view === 'list' ? ['offset', 'limit'] : ['change_id', 'part', 'start_byte', 'limit_bytes'])];
      } else {
        fields = {
          format_targets: ['preview_id', 'offset', 'limit'],
          format_request: ['quote_id', 'request_id', 'accept_cost', 'accept_model_processing'],
          format_status: ['batch_id'], format_results: ['batch_id', 'offset', 'limit'],
          format_restore: ['batch_id', 'target_id', 'expected_revision_id', 'request_id'],
          format_resume: ['batch_id'],
        }[op];
      }
      assertExactKeys(p, fields, error);
      for (const field of fields.filter((f) => f.endsWith('_id'))) {
        if (!id(p[field])) throw new Error(error);
      }
      if (p.selection !== undefined) {
        const s = p.selection;
        if (!s || !['item', 'group'].includes(s.kind)) throw new Error(error);
        assertExactKeys(s, s.kind === 'item' ? ['kind', 'item_id', 'revision_id', 'target']
          : ['kind', 'group_id', 'include_descendants'], error);
        if (s.kind === 'group') {
          if (!id(s.group_id) || typeof s.include_descendants !== 'boolean') throw new Error(error);
        } else {
          if (!id(s.item_id) || !id(s.revision_id) || !s.target
            || !['body', 'chapter'].includes(s.target.kind)) throw new Error(error);
          assertExactKeys(s.target, s.target.kind === 'body' ? ['kind'] : ['kind', 'side', 'chapter'], error);
          if (s.target.kind === 'chapter' && (!['original', 'translation'].includes(s.target.side)
            || typeof s.target.chapter !== 'string' || !s.target.chapter.trim()
            || Buffer.byteLength(s.target.chapter) > 128)) throw new Error(error);
        }
      }
      if (['format_targets', 'format_results'].includes(op) && !page(p.offset, p.limit, 100)) throw new Error(error);
      if (op === 'format_request' && (p.accept_cost !== true || p.accept_model_processing !== true)) throw new Error(error);
      if (op === 'format_changes') {
        if (typeof p.expected_version !== 'string' || !/^[a-f0-9]{64}$/.test(p.expected_version)) throw new Error(error);
        if (p.view === 'list') {
          if (!page(p.offset, p.limit, 64)) throw new Error(error);
        } else if (p.view !== 'text' || !['before', 'after'].includes(p.part)
          || !page(p.start_byte, p.limit_bytes, 8192)) throw new Error(error);
      }
      return { command: 'reader', max_sensitivity, principal, request: { op, ...p } };
    }
    case 'memex.library.reader.state':
    case 'memex.library.reader.save':
    case 'memex.library.reader.resume': {
      // B2: the existing owner UDS payloads (minus op). Ceiling and principal come only
      // from the authenticated session; the Store validates the state and anchors.
      const error = 'invalid_reader_state';
      const op = request.method.split('.').pop();
      if (op === 'save' && !grant.scopes.includes('edit')) throw new Error('permission_denied');
      assertExactKeys(p, op === 'resume' ? [] : op === 'state' ? ['item_id'] : ['item_id', 'expected_version', 'state'], error);
      if (op !== 'resume' && (typeof p.item_id !== 'string' || !/^[A-Za-z0-9_-]{1,256}$/.test(p.item_id))) throw new Error(error);
      if (op === 'save') {
        if (typeof p.expected_version !== 'string' || !/^[a-f0-9]{64}$/.test(p.expected_version)
          || !p.state || typeof p.state !== 'object' || Array.isArray(p.state)) throw new Error(error);
        assertExactKeys(p.state, ['visit_id', 'position', 'bookmarks'], error);
        if (!Array.isArray(p.state.bookmarks) || p.state.bookmarks.length > 4096) throw new Error(error);
      }
      return { command: 'reader', max_sensitivity, request: { op, ...p } };
    }
    case 'memex.library.reader.annotations.list':
    case 'memex.library.reader.annotations.edit': {
      // reader_annotations_v1: owner UDS payloads; ceiling/principal only from the session.
      const error = 'invalid_reader_annotation';
      const edit = request.method.endsWith('.edit');
      if (edit && (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal))) throw new Error('permission_denied');
      // reader_annotations_ai_translation_v1: an explicit, separate ledger. Absent means
      // exactly the edition-ledger v1 wire (no AI field accepted or forwarded).
      const ai = p && Object.hasOwn(p, 'annotation_space');
      if (ai && p.annotation_space !== 'ai_translation') throw new Error(error);
      const space = ai ? ['annotation_space'] : [];
      if (edit) assertExactKeys(p, ['item_id', 'expected_version', 'change', ...space], error);
      else assertExactKeys(p, ['item_id', 'offset', 'limit', 'expected_version', ...space,
        ...(ai ? ['projection', 'expected_view_version'].filter((k) => Object.hasOwn(p, k)) : [])], error);
      if (typeof p.item_id !== 'string' || !/^[A-Za-z0-9_-]{1,256}$/.test(p.item_id)) throw new Error(error);
      const hex = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
      const chapter = (v) => typeof v === 'string' && v.length > 0 && Buffer.byteLength(v) <= 128;
      if (!edit) {
        if (!Number.isSafeInteger(p.offset) || p.offset < 0 || !Number.isInteger(p.limit) || p.limit < 1 || p.limit > 100
          || !(p.expected_version === null ? p.offset === 0 : hex(p.expected_version))) throw new Error(error);
        if (ai) {
          const view = p.expected_view_version ?? null;
          if ((view === null) !== (p.expected_version === null) || (view !== null && !hex(view))) throw new Error(error);
          if (p.projection !== undefined && p.projection !== null) {
            assertExactKeys(p.projection, ['chapter', 'translation_version', 'sha256'], error);
            if (!chapter(p.projection.chapter) || !hex(p.projection.translation_version) || !hex(p.projection.sha256)) throw new Error(error);
          }
        }
        return { command: 'reader', max_sensitivity, request: { op: 'annotations_list', ...p } };
      }
      const c = p.change;
      if (!hex(p.expected_version) || !c || typeof c !== 'object' || Array.isArray(c)) throw new Error(error);
      if (c.op === 'delete') assertExactKeys(c, ['op', 'id'], error);
      else if (c.op === 'put' && ai) {
        assertExactKeys(c, ['op', 'annotation'], error);
        assertExactKeys(c.annotation || {}, ['id', 'style', 'anchor'], error);
        assertExactKeys(c.annotation.anchor || {}, ['revision_id', 'side', 'segments'], error);
        const segs = c.annotation.anchor.segments;
        if (!['bold', 'red', 'blue', 'underline', 'yellow'].includes(c.annotation.style)
          || c.annotation.anchor.side !== 'ai_translation'
          || !Array.isArray(segs) || segs.length < 1 || segs.length > 64) throw new Error(error);
        for (const sg of segs) {
          assertExactKeys(sg || {}, ['chapter', 'translation_version', 'sha256', 'start', 'end'], error);
          if (!chapter(sg.chapter) || !hex(sg.translation_version) || !hex(sg.sha256)
            || !Number.isSafeInteger(sg.start) || !Number.isSafeInteger(sg.end) || sg.start < 0 || sg.start >= sg.end) throw new Error(error);
        }
      } else if (c.op === 'put') {
        assertExactKeys(c, ['op', 'annotation'], error);
        assertExactKeys(c.annotation || {}, ['id', 'style', 'anchor'], error);
        assertExactKeys(c.annotation.anchor || {}, ['revision_id', 'side', 'sha256', 'segments'], error);
        const segs = c.annotation.anchor.segments;
        if (!['bold', 'red', 'blue', 'underline', 'yellow'].includes(c.annotation.style)
          || !Array.isArray(segs) || segs.length < 1 || segs.length > 64) throw new Error(error);
        for (const sg of segs) assertExactKeys(sg || {}, ['chapter', 'start', 'end'], error);
      } else throw new Error(error);
      if (Buffer.byteLength(JSON.stringify(p)) > 32 * 1024) throw new Error(error);
      return { command: 'reader', max_sensitivity, principal, request: { op: 'annotations_edit', ...p } };
    }
    case 'memex.library.reader.passes.list':
    case 'memex.library.reader.passes.edit':
    case 'memex.library.reader.passes.stats':
    case 'memex.library.reader.passes.timing_start':
    case 'memex.library.reader.passes.timing_report': {
      // reader_passes_v1: owner UDS payloads; ceiling and principal (the authenticated
      // device) come only from the session. Reads need read; all writes read+edit.
      const op = request.method.split('.').pop();
      const error = op === 'timing_report' ? 'invalid_reader_pass_timing' : 'invalid_reader_pass';
      const write = ['edit', 'timing_start', 'timing_report'].includes(op);
      if (write && (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal))) throw new Error('permission_denied');
      const id = (v) => typeof v === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(v);
      const passId = (v) => id(v) && /^pass_[A-Za-z0-9_-]+$/.test(v);
      const hex = (v) => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
      const nullable = (v, check) => v === null || check(v);
      const page = (max) => Number.isSafeInteger(p.offset) && p.offset >= 0 && Number.isInteger(p.limit) && p.limit >= 1 && p.limit <= max;
      const fields = {
        list: ['item_id', 'offset', 'limit', 'expected_version'],
        edit: ['item_id', 'expected_version', 'change'],
        stats: ['item_id', 'pass_id', 'offset', 'limit', 'expected_timing_version'],
        timing_start: ['item_id', 'pass_id'],
        timing_report: ['item_id', 'pass_id', 'session_id', 'segments'],
      }[op];
      assertExactKeys(p, fields, error);
      if (!id(p.item_id) || (p.pass_id !== undefined && !passId(p.pass_id))) throw new Error(error);
      const uop = `passes_${op}`;
      if (op === 'list') {
        if (!page(100) || !(p.expected_version === null ? p.offset === 0 : hex(p.expected_version))) throw new Error(error);
        return { command: 'reader', max_sensitivity, request: { op: uop, ...p } };
      }
      if (op === 'stats') {
        if (!page(400) || !(p.expected_timing_version === null ? p.offset === 0 : hex(p.expected_timing_version))) throw new Error(error);
        return { command: 'reader', max_sensitivity, request: { op: uop, ...p } };
      }
      if (op === 'timing_report') {
        const segs = p.segments;
        if (typeof p.session_id !== 'string' || !/^tsess_[A-Za-z0-9_-]{1,58}$/.test(p.session_id)
          || !Array.isArray(segs) || segs.length < 1 || segs.length > 64) throw new Error(error);
        for (const sg of segs) {
          assertExactKeys(sg, ['segment_id', 'mode', 'start_offset_ms', 'end_offset_ms'], error);
          if (!id(sg.segment_id) || !['source', 'bilingual', 'translation'].includes(sg.mode)
            || !Number.isSafeInteger(sg.start_offset_ms) || !Number.isSafeInteger(sg.end_offset_ms)) throw new Error(error);
        }
      }
      if (op === 'edit') {
        const c = p.change;
        const date = (d) => {
          if (d === null) return true;
          assertExactKeys(d, 'raw' in Object(d) ? ['value', 'precision', 'raw'] : ['value', 'precision'], error);
          return typeof d.value === 'string' && ['year', 'month', 'day'].includes(d.precision)
            && (d.raw === undefined || d.raw === null || typeof d.raw === 'string');
        };
        const method = (v) => v === null || ['full', 'sampled', 'structural'].includes(v);
        const minutes = (v) => v === null || (Number.isInteger(v) && v >= 1 && v <= 525600);
        const unit = (v) => v === null || ['minutes', 'hours'].includes(v);
        const zone = (v) => typeof v === 'string' && /^[A-Za-z0-9/_+-]{1,64}$/.test(v);
        const shapes = {
          create_tracked: ['pass_id', 'revision_id', 'method', 'start_date', 'timezone'],
          create_retrospective: ['pass_id', 'method', 'start_date', 'end_date', 'estimated_duration_minutes',
            'duration_display_unit', 'timezone', 'reference_revision_id'],
          update: ['pass_id', 'method', 'start_date', 'end_date', 'estimated_duration_minutes', 'duration_display_unit'],
          complete: ['pass_id', 'end_date', 'expected_timing_version'],
          reopen: ['pass_id'],
          delete: ['pass_id', 'expected_timing_version'],
        };
        if (!hex(p.expected_version) || !c || typeof c !== 'object' || Array.isArray(c) || !Object.hasOwn(shapes, c.op)) throw new Error(error);
        assertExactKeys(c, ['op', ...shapes[c.op]], error);
        if (!passId(c.pass_id)
          || ('revision_id' in c && !id(c.revision_id))
          || ('reference_revision_id' in c && !nullable(c.reference_revision_id, id))
          || ('method' in c && !method(c.method))
          || ['start_date', 'end_date'].some((k) => k in c && !date(c[k]))
          || ('estimated_duration_minutes' in c && !minutes(c.estimated_duration_minutes))
          || ('duration_display_unit' in c && !unit(c.duration_display_unit))
          || ('timezone' in c && !zone(c.timezone))
          || ('expected_timing_version' in c && !nullable(c.expected_timing_version, hex))) throw new Error(error);
      }
      if (Buffer.byteLength(JSON.stringify(p)) > 32 * 1024) throw new Error(error);
      return { command: 'reader', max_sensitivity, principal, request: { op: uop, ...p } };
    }
    case 'memex.library.reader.revision_chunk': {
      const error = 'invalid_reader_request';
      assertExactKeys(p, ['item_id', 'revision_id', 'target', 'start_byte', 'limit_bytes', 'expected_sha256'], error);
      if (typeof p.item_id !== 'string' || typeof p.revision_id !== 'string'
        || !p.target || !['body', 'chapter'].includes(p.target.kind)) throw new Error(error);
      assertExactKeys(p.target, p.target.kind === 'body' ? ['kind'] : ['kind', 'side', 'chapter'], error);
      if (p.target.kind === 'chapter' && (!['original', 'translation'].includes(p.target.side)
        || typeof p.target.chapter !== 'string' || !p.target.chapter.trim()
        || Buffer.byteLength(p.target.chapter) > 128)) throw new Error(error);
      if (!Number.isSafeInteger(p.start_byte) || p.start_byte < 0) throw new Error('invalid_reader_offset');
      if (!Number.isInteger(p.limit_bytes) || p.limit_bytes < 1 || p.limit_bytes > 8192) throw new Error('invalid_reader_limit');
      if (typeof p.expected_sha256 !== 'string' || !(p.expected_sha256 === 'any' || /^[a-f0-9]{64}$/.test(p.expected_sha256))) throw new Error(error);
      return { command: 'reader', max_sensitivity, request: { op: 'revision_chunk', ...p } };
    }
    case 'memex.library.reader.translate_preview':
    case 'memex.library.reader.translate_request':
    case 'memex.library.reader.translate_status':
    case 'memex.library.reader.translate_metadata':
    case 'memex.library.reader.translate_alignment':
    case 'memex.library.reader.translate_chunk': {
      const error = 'invalid_reader_translation_request';
      const op = request.method.split('.').pop();
      if (op === 'translate_request' && !grant.scopes.includes('edit')) throw new Error('permission_denied');
      const fields = op === 'translate_status' ? ['item_id', 'job_id'] : ['item_id', 'revision_id', 'chapter',
        ...(op === 'translate_request' ? ['request_id', 'quote_id', 'accept_cost', 'expected_translation_version']
          : ['translate_chunk', 'translate_alignment'].includes(op) ? ['expected_version', 'offset', 'limit'] : [])];
      assertExactKeys(p, fields, error);
      if (typeof p.item_id !== 'string') throw new Error(error);
      if (op === 'translate_status') {
        if (typeof p.job_id !== 'string' || !/^job_[A-Za-z0-9_-]{1,128}$/.test(p.job_id)) throw new Error(error);
      } else if (typeof p.revision_id !== 'string' || typeof p.chapter !== 'string'
        || !p.chapter.trim() || Buffer.byteLength(p.chapter) > 128) throw new Error(error);
      if (op === 'translate_request' && (p.accept_cost !== true || p.expected_translation_version !== 'missing'
        || typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.request_id)
        || typeof p.quote_id !== 'string' || !/^quote_[a-f0-9]{32}$/.test(p.quote_id))) throw new Error(error);
      if (op === 'translate_chunk' && (typeof p.expected_version !== 'string' || !/^[a-f0-9]{64}$/.test(p.expected_version)
        || !Number.isSafeInteger(p.offset) || p.offset < 0 || !Number.isInteger(p.limit) || p.limit < 4 || p.limit > 32768)) throw new Error(error);
      if (op === 'translate_alignment' && (typeof p.expected_version !== 'string' || !/^[a-f0-9]{64}$/.test(p.expected_version)
        || !Number.isSafeInteger(p.offset) || p.offset < 0 || !Number.isInteger(p.limit) || p.limit < 1 || p.limit > 256)) throw new Error(error);
      return { command: 'reader', max_sensitivity, request: { op, ...p } };
    }
    case 'memex.library.reader.edit_begin':
    case 'memex.library.reader.edit_chunk':
    case 'memex.library.reader.edit_commit': {
      if (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) throw new Error('permission_denied');
      const error = 'invalid_reader_edit';
      const op = request.method.split('.').pop();
      const fields = op === 'edit_begin'
        ? ['item_id', 'revision_id', 'side', 'chapter', 'expected_chapter_sha256', 'request_id', 'bytes', 'sha256']
        : ['item_id', 'edit_id', ...(op === 'edit_chunk' ? ['offset', 'markdown'] : [])];
      assertExactKeys(p, fields, error);
      if (typeof p.item_id !== 'string') throw new Error(error);
      if (op === 'edit_begin') {
        if (typeof p.revision_id !== 'string' || !['original', 'translation'].includes(p.side)
          || typeof p.chapter !== 'string' || !p.chapter.trim() || Buffer.byteLength(p.chapter) > 128
          || typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.request_id)
          || typeof p.expected_chapter_sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(p.expected_chapter_sha256)
          || typeof p.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(p.sha256)
          || !Number.isSafeInteger(p.bytes) || p.bytes < 1) throw new Error(error);
        if (p.bytes > 16 * 1024 * 1024) throw new Error('reader_edit_too_large');
      } else if (typeof p.edit_id !== 'string' || !/^edit_[a-f0-9]{64}$/.test(p.edit_id)) throw new Error(error);
      if (op === 'edit_chunk' && (!Number.isSafeInteger(p.offset) || p.offset < 0
        || typeof p.markdown !== 'string' || !p.markdown || p.markdown.includes('\0')
        || Buffer.byteLength(p.markdown) > 8192)) throw new Error(error);
      return { command: 'reader', max_sensitivity, principal, request: { op, ...p } };
    }
    case 'memex.library.reader.align_request': {
      if (!grant.scopes.includes('edit')) throw new Error('permission_denied');
      const error = 'invalid_reader_alignment_request';
      assertExactKeys(p, ['item_id', 'revision_id', 'chapter', 'request_id', 'expected_alignment_version'], error);
      if (typeof p.item_id !== 'string' || typeof p.revision_id !== 'string'
        || typeof p.chapter !== 'string' || !p.chapter.trim() || p.chapter.length > 128
        || typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.request_id)
        || !(p.expected_alignment_version === 'missing' || /^[a-f0-9]{64}$/.test(p.expected_alignment_version))) throw new Error(error);
      return { command: 'reader', max_sensitivity, request: { op: 'align_request', ...p } };
    }
    case 'memex.library.reader.align_status': {
      const error = 'invalid_reader_alignment_request';
      assertExactKeys(p, ['item_id', 'job_id'], error);
      if (typeof p.item_id !== 'string' || typeof p.job_id !== 'string'
        || !/^job_[A-Za-z0-9_-]{1,128}$/.test(p.job_id)) throw new Error(error);
      return { command: 'reader', max_sensitivity, request: { op: 'align_status', ...p } };
    }
    case 'memex.library.reader.alignment': {
      const error = 'invalid_reader_alignment_request';
      assertExactKeys(p, ['item_id', 'revision_id', 'chapter', 'offset', 'limit', 'expected_version'], error);
      if (typeof p.item_id !== 'string' || typeof p.revision_id !== 'string'
        || typeof p.chapter !== 'string' || !p.chapter.trim() || p.chapter.length > 128
        || !Number.isSafeInteger(p.offset) || p.offset < 0
        || !Number.isInteger(p.limit) || p.limit < 1 || p.limit > 256
        || typeof p.expected_version !== 'string'
        || !(p.expected_version === 'missing' || /^[a-f0-9]{64}$/.test(p.expected_version)
          || (p.expected_version === 'any' && p.offset === 0))) throw new Error(error);
      return { command: 'reader', max_sensitivity, request: { op: 'alignment', ...p } };
    }
    case 'memex.library.reader.toc':
    case 'memex.library.reader.chunk': {
      const op = request.method.endsWith('.toc') ? 'toc' : 'chunk';
      assertExactKeys(p, ['item_id', 'revision_id',
        ...(op === 'chunk' ? ['side', 'chapter', 'offset', 'limit'] : [])], 'invalid_reader_request');
      if (typeof p.item_id !== 'string' || typeof p.revision_id !== 'string') throw new Error('invalid_reader_request');
      if (op === 'chunk') {
        if (!['original', 'translation', 'source'].includes(p.side)
          || typeof p.chapter !== 'string' || p.chapter.trim().length === 0
          || p.chapter.length > 256) throw new Error('invalid_reader_request');
        if (!Number.isSafeInteger(p.offset) || p.offset < 0) throw new Error('invalid_reader_offset');
        if (!Number.isInteger(p.limit) || p.limit < 4 || p.limit > 32768) throw new Error('invalid_reader_limit');
      }
      return { command: 'reader', max_sensitivity, request: { op, ...p } };
    }
    case 'memex.library.item_cover_batch':
      assertExactKeys(p, ['item_ids'], 'invalid_cover_request');
      if (!Array.isArray(p.item_ids) || p.item_ids.length > 50
        || p.item_ids.some((id) => typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,256}$/.test(id))) {
        throw new Error('invalid_cover_request');
      }
      return { command: 'item_cover_batch', ...p, max_sensitivity };
    case 'memex.library.item_cover_chunk':
      assertExactKeys(p, ['item_id', 'version', 'thumbnail', 'offset'], 'invalid_cover_request');
      if (typeof p.item_id !== 'string' || typeof p.version !== 'string' || !/^[a-f0-9]{64}$/.test(p.version)
        || typeof p.thumbnail !== 'boolean' || !Number.isSafeInteger(p.offset) || p.offset < 0) {
        throw new Error('invalid_cover_request');
      }
      return { command: 'item_cover_chunk', ...p, max_sensitivity };
    case 'memex.library.image.read':
      assertExactKeys(p, ['item_id', 'expected_revision_id', 'target', 'offset'], 'invalid_request');
      if (typeof p.item_id !== 'string' || typeof p.expected_revision_id !== 'string'
        || typeof p.target !== 'string' || !/^attachments\/(sha256-)?[a-f0-9]{64}\.(png|jpe?g|gif|webp)$/.test(p.target)
        || !Number.isSafeInteger(p.offset) || p.offset < 0) throw new Error('invalid_request');
      return { command: 'image_read', ...p, max_sensitivity };
    case 'memex.capture.enqueue':
    case 'memex.capture.status': {
      if (!grant.scopes.includes('capture') || max_sensitivity === 'public'
        || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) {
        throw new Error('permission_denied');
      }
      if (request.method === 'memex.capture.enqueue') {
        assertExactKeys(p, ['request_key', 'url'], 'invalid_request');
        if (typeof p.request_key !== 'string' || !/^remote_mobile_[A-Za-z0-9_-]{32}$/.test(p.request_key)
          || typeof p.url !== 'string' || !p.url.startsWith('https://')
          || Buffer.byteLength(p.url) > 8192) throw new Error('invalid_request');
        return { command: 'scoped_capture_enqueue', ...p, principal, max_sensitivity };
      }
      assertExactKeys(p, ['job_id'], 'invalid_request');
      if (typeof p.job_id !== 'string' || !/^job_[a-f0-9]{32}$/.test(p.job_id)) throw new Error('invalid_request');
      return { command: 'scoped_capture_status', ...p, principal, max_sensitivity };
    }
    case 'memex.library.translation.status':
      assertExactKeys(p, ['item_id'], 'invalid_request');
      return { command: 'scoped_translation_status', ...p, max_sensitivity };
    case 'memex.library.translation.read':
      assertExactKeys(p, ['item_id', 'expected_revision_id', 'start_byte'], 'invalid_request');
      if (!Number.isSafeInteger(p.start_byte) || p.start_byte < 0) throw new Error('invalid_context_range');
      return { command: 'scoped_translation_read', ...p, max_sensitivity };
    case 'memex.library.minutes.status':
      assertExactKeys(p, ['item_id'], 'invalid_request');
      return { command: 'scoped_minutes_status', ...p, max_sensitivity };
    case 'memex.library.insights.status':
      assertExactKeys(p, ['item_id'], 'invalid_request');
      return { command: 'scoped_insights_status', ...p, max_sensitivity };
    case 'memex.library.insights.request':
      // Same nature and scope as minutes/translation requests: explicit, starts provider work.
      if (!grant.scopes.includes('capture')) throw new Error('permission_denied');
      assertExactKeys(p, ['item_id', 'kinds'], 'invalid_request');
      if (!Array.isArray(p.kinds) || p.kinds.length < 1 || p.kinds.length > INSIGHT_KINDS.length
        || p.kinds.some((kind) => !INSIGHT_KINDS.includes(kind))
        || new Set(p.kinds).size !== p.kinds.length) throw new Error('invalid_request');
      return { command: 'insight_request', item_id: p.item_id, max_sensitivity, kinds: [...p.kinds] };
    case 'memex.library.trash.list':
      assertExactKeys(p, ['group_id', 'cursor'], 'invalid_request');
      if (typeof p.group_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.group_id)
        || !(p.cursor === null || (typeof p.cursor === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(p.cursor)))) throw new Error('invalid_request');
      return { command: 'group_trash_list', ...p, max_sensitivity };
    case 'memex.library.trash.edit': {
      if (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) throw new Error('permission_denied');
      assertExactKeys(p, ['group_id', 'item_id', 'expected_revision_id', 'request_id', 'restore_plan'], 'invalid_request');
      if (typeof p.group_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(p.group_id)
        || typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{16,64}$/.test(p.request_id)
        || typeof p.item_id !== 'string' || typeof p.expected_revision_id !== 'string'
        || !(p.restore_plan === null || (typeof p.restore_plan === 'string' && /^[A-Za-z0-9_-]{1,256}$/.test(p.restore_plan)))) throw new Error('invalid_request');
      const request_id = `mobile-trash-${createHash('sha256').update(`${principal}:${p.request_id}`).digest('hex')}`;
      return { command: 'group_trash_edit', ...p, request_id, max_sensitivity };
    }
    case 'memex.library.groups.reorder': {
      if (!grant.scopes.includes('edit') || typeof principal !== 'string' || !/^[a-f0-9]{64}$/.test(principal)) {
        throw new Error('permission_denied');
      }
      assertExactKeys(p, ['request_id', 'group_id', 'before_id', 'expected_version'], 'invalid_request');
      const groupId = (v) => typeof v === 'string' && /^[A-Za-z0-9_:.-]{1,128}$/.test(v);
      if (typeof p.request_id !== 'string' || !/^[A-Za-z0-9_-]{16,64}$/.test(p.request_id)
        || !groupId(p.group_id) || !(p.before_id === null || groupId(p.before_id))
        || typeof p.expected_version !== 'string' || !/^v[0-9]{1,20}$/.test(p.expected_version)) {
        throw new Error('invalid_request');
      }
      // Namespace durable retry identities by authenticated principal, not by a phone-provided identity.
      return { command: 'group_edit', max_sensitivity, request_id: `mobile-reorder-${principal}-${p.request_id}`,
        expected_version: p.expected_version, ops: [{ op: 'reorder_root', group_id: p.group_id, before_id: p.before_id }] };
    }
    case 'memex.library.groups.tree':
      // The authorized group tree (preorder) under the session ceiling; read-only.
      assertExactKeys(p, ['offset', 'limit', 'expected_view'], 'invalid_request');
      if (!Number.isSafeInteger(p.offset) || p.offset < 0 || !Number.isInteger(p.limit) || p.limit < 1 || p.limit > 500
        || !(p.expected_view === null || (typeof p.expected_view === 'string' && p.expected_view.length <= 256))) {
        throw new Error('invalid_request');
      }
      return { command: 'group_tree_read', max_sensitivity, offset: p.offset, limit: p.limit, expected_view: p.expected_view };
    case 'memex.library.conversation.read':
      // The stored item_chat turns, oldest first; `cursor` is the previous page's next_cursor.
      assertExactKeys(p, ['item_id', 'cursor'], 'invalid_request');
      // Restored turns (owner-only conversation_import, never routed here) have negative seq.
      if (!(p.cursor === null || (typeof p.cursor === 'string' && /^-?[0-9]{1,18}$/.test(p.cursor)))) {
        throw new Error('invalid_request');
      }
      return { command: 'conversation_read', item_id: p.item_id, max_sensitivity, cursor: p.cursor };
    case 'memex.library.minutes.request':
    case 'memex.library.translation.request':
      // An explicit user action that starts provider work for the item's current revision.
      // `capture` is the grant for starting Mac-side jobs; `edit` stays for owner metadata CAS.
      if (!grant.scopes.includes('capture')) throw new Error('permission_denied');
      assertExactKeys(p, ['item_id'], 'invalid_request');
      return { command: request.method === 'memex.library.minutes.request' ? 'minutes_request' : 'translation_request',
        ...p, max_sensitivity };
    case 'memex.library.item.chat':
      // Item chat sends the item and the conversation to the owner's chat provider; the
      // daemon enforces the item ceiling, message bounds and the request id format.
      assertExactKeys(p, ['item_id', 'request_id', 'messages'], 'invalid_request');
      if (typeof p.request_id !== 'string' || !/^[\x21-\x7e]{16,128}$/.test(p.request_id)
        || !Array.isArray(p.messages) || p.messages.length < 1 || p.messages.length > 40
        || p.messages.some((m) => !m || typeof m !== 'object' || Array.isArray(m)
          || Object.keys(m).sort().join() !== 'content,role'
          || !['user', 'assistant'].includes(m.role) || typeof m.content !== 'string')) {
        throw new Error('invalid_request');
      }
      return { command: 'item_chat', item_id: p.item_id, max_sensitivity, request_id: p.request_id,
        messages: p.messages };
    case 'memex.session.info':
      assertExactKeys(p, [], 'invalid_request');
      return null;
    case 'memex.library.query':
      assertExactKeys(p, ['query', 'offset', 'limit', 'expected_view'], 'invalid_request');
      // groups_v1: optional node filter or the global Inbox predicate.
      assertExactKeys(p.query, ['text', 'source', 'item_type', 'sort', 'descending',
        ...('group_id' in Object(p.query) ? ['group_id'] : []),
        ...('include_descendants' in Object(p.query) ? ['include_descendants'] : []),
        ...('unfiled' in Object(p.query) ? ['unfiled'] : []),
        ...('origin' in Object(p.query) ? ['origin'] : [])], 'invalid_request');
      if (p.query.group_id !== undefined && (typeof p.query.group_id !== 'string'
        || !/^[A-Za-z0-9_:.-]{1,256}$/.test(p.query.group_id) || typeof p.query.include_descendants !== 'boolean')) {
        throw new Error('invalid_request');
      }
      if (typeof p.query.text !== 'string' || Buffer.byteLength(p.query.text) > 4096
        || ![p.query.source, p.query.item_type].every((v) => v === null || typeof v === 'string')
        || ('include_descendants' in p.query && typeof p.query.include_descendants !== 'boolean')
        || (p.query.include_descendants === true && p.query.group_id === undefined)
        || ('unfiled' in p.query && typeof p.query.unfiled !== 'boolean')
        || (p.query.unfiled === true && (p.query.group_id !== undefined || p.query.include_descendants === true))
        || ![undefined, null, 'capture', 'import'].includes(p.query.origin)
        || !QUERY_SORTS.includes(p.query.sort)
        || typeof p.query.descending !== 'boolean'
        || !Number.isSafeInteger(p.offset) || p.offset < 0
        || !Number.isInteger(p.limit) || p.limit < 1 || p.limit > 200
        || !(p.expected_view === null || typeof p.expected_view === 'string')) {
        throw new Error('invalid_request');
      }
      return { command: 'library_query', ...p, max_sensitivity };
    case 'memex.library.item.get':
      assertExactKeys(p, ['item_id'], 'invalid_request');
      return { command: 'scoped_item_read', ...p, max_sensitivity };
    case 'memex.library.context.read':
      assertExactKeys(p, ['item_id', 'expected_revision_id', 'start_byte', 'end_byte'], 'invalid_request');
      if (!Number.isSafeInteger(p.start_byte) || !Number.isSafeInteger(p.end_byte)
        || p.start_byte < 0 || p.end_byte <= p.start_byte
        || p.end_byte - p.start_byte > 16 * 1024) throw new Error('invalid_context_request');
      return { command: 'context_read', ...p, max_sensitivity };
    case 'memex.library.metadata.update':
      if (!grant.scopes.includes('edit')) throw new Error('permission_denied');
      assertExactKeys(p, ['item_id', 'expected_revision_id', 'title', 'manual_tags'], 'invalid_request');
      if (typeof p.title !== 'string' || !Array.isArray(p.manual_tags)
        || p.manual_tags.some((tag) => typeof tag !== 'string')) throw new Error('invalid_request');
      return {
        command: 'scoped_metadata_update', item_id: p.item_id, max_sensitivity,
        update: { expected_revision_id: p.expected_revision_id, title: p.title, manual_tags: p.manual_tags },
      };
    default: throw new Error('invalid_request');
  }
}

function createLibraryRouter({ socketPath, authorization, sendCommand = sendOwnerCommand, webBundle = null }) {
  if (!socketPath || !(authorization instanceof SessionGrants)) throw new Error('invalid_gateway');
  return async (raw, grant, principal) => {
    let id = null;
    let writeDispatched = false;
    try {
      authorization.assertCurrent(grant || {}, grant);
      if (!grant.scopes.includes('read')) throw new Error('permission_denied');
      const bytes = Buffer.from(raw);
      if (bytes.length === 0 || bytes.length > MAX_REQUEST_BYTES) throw new Error('invalid_request');
      const request = JSON.parse(bytes.toString('utf8'));
      assertExactKeys(request, ['id', 'method', 'params'], 'invalid_request');
      if (typeof request.id !== 'string' || !/^[A-Za-z0-9_.-]{1,128}$/.test(request.id)) throw new Error('invalid_request');
      id = request.id;
      if (!METHODS.has(request.method)) throw new Error('invalid_request');
      for (const field of ['item_id', 'expected_revision_id', 'revision_id']) {
        if (request.params?.[field] !== undefined
          && (typeof request.params[field] !== 'string'
            || !/^[A-Za-z0-9_-]{1,256}$/.test(request.params[field]))) throw new Error('invalid_request');
      }
      const command = commandFor(request, grant, principal);
      let result;
      if (command === null) {
        const [capabilities, agent] = await Promise.all([
          sendCommand(socketPath, { command: 'capabilities' })
            .then((r) => r?.ok ? r.capabilities : null, () => null),
          sendCommand(socketPath, { command: 'agent_version' })
            .then((r) => r?.ok ? r.agent_version : null, () => null),
        ]);
        result = {
          scopes: grant.scopes, max_sensitivity: grant.maxSensitivity,
          expires_at_ms: grant.expiresAt, generation_ready: false,
          chat: 'available', translation: 'available', minutes: 'available',
          insights: 'available', conversation: 'available', groups: 'available',
          groups_v1: { query_unfiled: capabilities?.groups_v1?.query_unfiled === true, node_delete: false },
          item_cover_v1: Array.isArray(capabilities?.features) && capabilities.features.includes('item_cover_v1')
            ? 'available' : 'unavailable',
          reader: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_v1')
            ? 'available' : 'unavailable',
          reader_alignment: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_paragraph_alignment_v1')
            ? 'available' : 'unavailable',
          reader_repair: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_chapter_repair_v1')
            ? 'available' : 'unavailable',
          reader_repair_batch: 'unavailable',
          reader_translation_map_repair: 'unavailable',
          reader_edit: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_chapter_edit_v1')
            ? 'available' : 'unavailable',
          reader_translation: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_chapter_translation_v1')
            ? 'available' : 'unavailable',
          reader_translation_metadata: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_chapter_translation_metadata_v1')
            ? 'available' : 'unavailable',
          reader_translation_alignment: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_chapter_translation_alignment_v1')
            ? 'available' : 'unavailable',
          reader_revision_text: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_revision_text_v1')
            ? 'available' : 'unavailable',
          reader_format: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_content_format_v1')
            ? 'available' : 'unavailable',
          reader_state_sync_v1: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_v1')
            ? 'available' : 'unavailable',
          reader_annotations_v1: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_annotations_v1')
            ? 'available' : 'unavailable',
          reader_passes_v1: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_passes_v1')
            ? 'available' : 'unavailable',
          reader_annotations_ai_translation_v1: Array.isArray(capabilities?.features)
            && capabilities.features.includes('reader_annotations_ai_translation_v1') ? 'available' : 'unavailable',
          reader_format_batch: Array.isArray(capabilities?.features) && capabilities.features.includes('reader_content_format_batch_v1')
            ? 'available' : 'unavailable',
          query_sorts: [...QUERY_SORTS],
          query_origin: agent?.query_origin === true ? 'available' : 'unavailable',
          group_reorder: agent?.group_reorder === true && grant.scopes.includes('edit') ? 'available' : 'unavailable',
          ui_active_group_v1: capabilities?.features?.includes('ui_active_group_v1') && grant.scopes.includes('edit') ? 'available' : 'unavailable',
          group_trash: agent?.group_trash === true ? 'available' : 'unavailable',
          // Build identities only (no paths or secrets). `agent` is null when the daemon is
          // unreachable or predates agent_version.
          versions: {
            gateway: ROUTER_SHA256, web_bundle: webBundle,
            agent: agent ? { version: agent.version, binary_sha256: agent.binary_sha256 ?? null } : null,
          },
        };
      } else {
        writeDispatched = ['scoped_metadata_update', 'scoped_capture_enqueue', 'group_edit', 'group_trash_edit', 'ui_active_group_set'].includes(command.command)
          || REQUEST_COMMANDS.has(command.command)
          || (command.command === 'reader' && ['align_request', 'translate_request', 'edit_begin', 'edit_chunk', 'edit_commit',
            'format_request', 'format_restore', 'format_resume', 'save', 'annotations_edit',
            'passes_edit', 'passes_timing_start', 'passes_timing_report'].includes(command.request.op));
        const response = command.command === 'item_chat'
          ? await sendCommand(socketPath, command, { timeoutMs: CHAT_TIMEOUT_MS })
          : await sendCommand(socketPath, command);
        authorization.assertCurrent(grant, grant);
        if (!response?.ok) throw new Error(response?.reason_code || 'internal_error');
        if (command.command === 'translation_request') {
          // Translation projection (no page) plus the minutes it reports but never authorizes.
          const t = response.translation;
          result = t && typeof t === 'object' ? {
            state: t.state, reason: t.reason ?? null, revision_id: t.revision_id,
            translated_title: t.translated_title ?? null, progress: t.progress ?? null,
            ai_minutes: response.ai_minutes ?? null,
          } : null;
        } else if (command.command === 'insight_request') {
          // States only: the items are read with insights.status (bounded there, never here).
          const a = response.ai_insights;
          result = a && typeof a === 'object' ? Object.fromEntries(INSIGHT_KINDS.map((kind) => [kind, {
            state: a[kind]?.state, reason: a[kind]?.reason ?? null, dropped: a[kind]?.dropped ?? 0,
          }])) : null;
        } else if (command.command === 'scoped_insights_status') {
          result = response.ai_insights;
        } else if (command.command === 'image_read') {
          result = response.image;
        } else if (command.command === 'item_cover_batch' || command.command === 'item_cover_chunk') {
          result = response.item_cover;
        } else if (command.command === 'reader') {
          result = response.reader;
        } else if (command.command === 'conversation_read') {
          result = response.conversation;
        } else if (command.command === 'group_trash_list') {
          result = { items: response.trash_items, next_cursor: response.next_cursor ?? null };
        } else if (command.command === 'group_trash_edit') {
          result = { applied: true };
        } else if (command.command === 'group_edit') {
          result = response.group_edit;
        } else if (command.command === 'ui_active_group_set') {
          const selected = response.ui_active_group;
          result = selected ? { schema_version: selected.schema_version, request_id: selected.request_id,
            group_id: selected.group_id ?? null, updated_at: selected.updated_at } : null;
        } else if (command.command === 'group_tree_read') {
          const t = response.group_tree;
          result = t && Array.isArray(t.nodes) ? { view: t.view, total: t.total, offset: t.offset,
            next_offset: t.next_offset ?? null,
            nodes: t.nodes.map((n) => ({ id: n.id, name: n.name, kind: n.kind, parent_id: n.parent_id ?? null,
              root_group_id: n.root_group_id, version: n.version, read_only: n.read_only,
              item_count: n.item_count, subtree_item_count: n.subtree_item_count })) } : null;
        } else {
          result = response.library_query ?? response.library_entry ?? response.context
            ?? response.item_update ?? response.translation ?? response.item_chat ?? response.ai_minutes;
        }
        if (response.job) {
          const { job_id, state, item_id } = response.job;
          result = { job_id, state, item_id };
        }
        if (!result || typeof result !== 'object') throw new Error('internal_error');
      }
      const response = { id, result };
      if (Buffer.byteLength(JSON.stringify(response)) > MAX_PLAINTEXT_BYTES) throw new Error('result_too_large');
      return response;
    } catch (error) {
      // Store rejection codes below precede mutation. Any other failure after
      // dispatch may have lost a durable receipt, including a UDS timeout while
      // the authenticated WebSocket remains connected.
      const knownWriteRejection = new Set(['library_resource_not_found', 'stale_item_revision',
        'invalid_content_format_request', 'content_format_unavailable', 'content_format_preview_not_found',
        'content_format_quote_stale', 'content_format_quote_consumed', 'content_format_batch_not_found',
        'content_format_changes_stale', 'content_format_restore_conflict',
        'invalid_reader_translation_request', 'reader_translation_not_needed', 'reader_translation_language_unsupported',
        'reader_translation_budget_exceeded', 'reader_translation_quote_stale', 'reader_translation_unavailable',
        'stale_reader_translation', 'no_translation_provider',
        'reader_job_in_progress', 'stale_chapter_alignment', 'no_low_dial_provider',
        'invalid_reader_alignment_request',
        'invalid_reader_edit', 'reader_edit_not_found', 'reader_edit_too_large', 'reader_edit_upload_conflict',
        'reader_side_not_found', 'reader_chapter_not_found',
        'invalid_reader_state', 'invalid_reader_anchor', 'stale_reader_state',
        'invalid_reader_annotation', 'stale_reader_annotations', 'reader_annotation_conflict',
        'reader_annotation_limit_exceeded', 'reader_annotation_not_found',
        'invalid_reader_pass', 'invalid_reader_pass_dates', 'invalid_reader_pass_timing', 'stale_reader_passes', 'stale_reader_pass_timing', 'reader_pass_conflict', 'reader_pass_not_found', 'reader_pass_limit_exceeded', 'reader_pass_closed', 'reader_pass_not_timeable', 'reader_pass_session_expired', 'reader_pass_clock_discontinuity', 'reader_pass_segment_conflict',
        'invalid_metadata_update', 'metadata_unchanged', 'item_revision_limit_exceeded',
        'invalid_request', 'permission_denied', 'remote_resource_not_found', 'request_identity_conflict',
        'invalid_group_edit', 'stale_group_version', 'groups_unchanged', 'request_id_conflict',
        'group_limit_exceeded', 'group_request_limit_exceeded',
        'invalid_trash_request', 'item_already_trashed', 'item_not_trashed',
        'no_minutes_provider', 'no_insight_provider', 'invalid_insight_request']);
      const code = writeDispatched && !knownWriteRejection.has(error.message) ? 'unknown'
        : (ERRORS.has(error.message) ? error.message : 'invalid_request');
      return { id, error: { code, message: code } };
    }
  };
}

module.exports = { SessionGrants, createLibraryRouter };
