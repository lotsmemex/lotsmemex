'use strict';

const assert = require('node:assert/strict');
const { once } = require('node:events');
const { webcrypto, createHash } = require('node:crypto');
const test = require('node:test');
const { WebSocket } = require('ws');
const { clientProtocols } = require('../admission');
const { sendOwnerCommand } = require('../mac/ipc-client');
const { startMobileFixture } = require('./mobile-fixture');
const fs = require('node:fs');
const path = require('node:path');
const { spawn, execFile } = require('node:child_process');
const { promisify } = require('node:util');

const query = { text: '', source: null, item_type: null, sort: 'title', descending: false };
const window = { query, offset: 0, limit: 1, expected_view: null };

test('active group selection requires edit and binds principal and ceiling', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'selection', generation: 1,
    scopes: ['read', 'edit'], maxSensitivity: 'public', expiresAt: 10000 });
  const commands = [];
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async (_, command) => {
      commands.push(command);
      return { ok: true, ui_active_group: { schema_version: 1, request_id: command.request_id,
        group_id: command.group_id, updated_at: '2026-11-08T00:00:00Z' } };
    } });
  const params = { request_id: 'selection_request_001', group_id: 'group_import_root' };
  const call = (p, principal = 'a'.repeat(64), g = grant) => route(JSON.stringify({
    id: 'selection', method: 'memex.library.ui.active_group.set', params: p }), g, principal);
  assert.equal((await call(params)).result.group_id, params.group_id);
  const first = commands.at(-1);
  assert.equal(first.max_sensitivity, 'public');
  assert.equal(first.command, 'ui_active_group_set');
  await call(params);
  assert.equal(commands.at(-1).request_id, first.request_id);
  await call(params, 'b'.repeat(64));
  assert.notEqual(commands.at(-1).request_id, first.request_id);
  assert.equal((await call({ ...params, group_id: null })).result.group_id, null);
  const count = commands.length;
  for (const p of [{ ...params, max_sensitivity: 'restricted' }, { ...params, group_id: '../bad' }]) {
    assert.equal((await call(p)).error.code, 'invalid_request');
  }
  assert.equal((await call(params, '')).error.code, 'permission_denied');
  const read = authorization.set({ sessionId: 'read-selection', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  assert.equal((await call(params, 'a'.repeat(64), read)).error.code, 'permission_denied');
  assert.equal(commands.length, count);
});

test('item cover read DTO is bounded, session-scoped and separately advertised', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'covers', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  const commands = [];
  let supported = false;
  let failure;
  const chunk = { media_type: 'image/png', sha256: 'a'.repeat(64), total_bytes: 3,
    offset: 0, next_offset: null, data_base64: 'AQID' };
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async (_, command) => {
      commands.push(command);
      if (failure) return { ok: false, reason_code: failure };
      return { ok: true, capabilities: { features: supported ? ['item_cover_v1'] : [] },
        item_cover: command.command === 'item_cover_batch' ? [null] : chunk };
    } });
  const call = (method, params) => route(JSON.stringify({ id: 'cover', method, params }), grant);
  assert.equal((await call('memex.session.info', {})).result.item_cover_v1, 'unavailable');
  supported = true;
  assert.equal((await call('memex.session.info', {})).result.item_cover_v1, 'available');
  const batchMethod = 'memex.library.item_cover_batch';
  const chunkMethod = 'memex.library.item_cover_chunk';
  assert.deepEqual((await call(batchMethod, { item_ids: ['item_test'] })).result, [null]);
  assert.equal(commands.at(-1).max_sensitivity, 'public');
  const params = { item_id: 'item_test', version: 'b'.repeat(64), thumbnail: true, offset: 0 };
  assert.deepEqual((await call(chunkMethod, params)).result, chunk);
  assert.deepEqual(commands.at(-1), { command: 'item_cover_chunk', ...params, max_sensitivity: 'public' });
  const before = commands.length;
  for (const bad of [{ item_ids: Array(51).fill('item_test') }, { item_ids: ['../outside'] },
    { item_ids: ['item_test'], max_sensitivity: 'private' }]) {
    assert.equal((await call(batchMethod, bad)).error.code, 'invalid_cover_request');
  }
  for (const bad of [{ ...params, thumbnail: 1 }, { ...params, version: 'any' },
    { ...params, offset: -1 }, { ...params, max_sensitivity: 'private' }]) {
    assert.equal((await call(chunkMethod, bad)).error.code, 'invalid_cover_request');
  }
  assert.equal(commands.length, before);
  failure = 'stale_cover_version';
  assert.equal((await call(chunkMethod, params)).error.code, failure);
  failure = 'private /path/to/library';
  assert.deepEqual((await call(chunkMethod, params)).error,
    { code: 'invalid_request', message: 'invalid_request' });
});

test('Inbox boolean is forwarded strictly and capability fails closed', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'inbox', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  const commands = [];
  let supported = true;
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async (_, command) => {
      commands.push(command);
      return { ok: true, capabilities: supported ? { groups_v1: { query_unfiled: true } } : {} };
    } });
  const call = (method, params) => route(JSON.stringify({ id: 'inbox', method, params }), grant);
  assert.equal((await call('memex.session.info', {})).result.groups_v1.query_unfiled, true);
  supported = false;
  assert.equal((await call('memex.session.info', {})).result.groups_v1.query_unfiled, false);
  for (const unfiled of [true, false]) {
    await call('memex.library.query', { ...window, query: { ...query, unfiled, include_descendants: false } });
    assert.equal(commands.at(-1).query.unfiled, unfiled);
    assert.equal(commands.at(-1).max_sensitivity, 'public');
  }
  const before = commands.length;
  for (const extra of [{ unfiled: null }, { unfiled: 1 }, { unfiled: 'true' },
    { unfiled: true, group_id: 'g', include_descendants: false },
    { unfiled: true, include_descendants: true }, { unfiled: true, cluster_id: 'g' }]) {
    assert.equal((await call('memex.library.query', { ...window, query: { ...query, ...extra } })).error.code, 'invalid_request');
  }
  assert.equal(commands.length, before);
});

test('new batch and map workers stay remote-disabled even if Rust advertises them', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'new-work', generation: 1,
    scopes: ['read', 'edit'], maxSensitivity: 'private', expiresAt: 10000 });
  const commands = [];
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async (_, command) => {
      commands.push(command);
      return { ok: true, capabilities: { features: ['reader_chapter_repair_batch_v1',
        'reader_translation_map_repair_v1', 'reader_chapter_repair_v1'], groups_v1: { node_delete: true } } };
    } });
  const call = (method) => route(JSON.stringify({ id: 'gate', method, params: {} }), grant, 'a'.repeat(64));
  const info = (await call('memex.session.info')).result;
  assert.equal(info.reader_repair, 'available');
  assert.equal(info.reader_repair_batch, 'unavailable');
  assert.equal(info.reader_translation_map_repair, 'unavailable');
  assert.equal(info.groups_v1.node_delete, false);
  const before = commands.length;
  for (const op of ['delete_preview', 'delete', 'delete_list', 'delete_restore']) {
    assert.equal((await call(`memex.library.groups.${op}`)).error.code, 'permission_denied');
  }
  for (const op of ['align_batch_preview', 'align_batch_start', 'align_batch_status', 'align_batch_stop',
    'translate_map_preview', 'translate_map_request', 'translate_map_status', 'translate_map_stop', 'translate_map_read']) {
    assert.equal((await call(`memex.library.reader.${op}`)).error.code, 'permission_denied');
  }
  assert.equal(commands.length, before);
});

test('revision text admission is read-only and rejects injected scope and oversized chunks', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'history', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  const commands = [];
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async (_, command) => { commands.push(command); return { ok: false, reason_code: 'revision_text_unavailable' }; } });
  const params = { item_id: 'item_test', revision_id: 'revision_test', target: { kind: 'body' },
    start_byte: 0, limit_bytes: 8192, expected_sha256: 'any' };
  const request = (p) => route(JSON.stringify({ id: 'history', method: 'memex.library.reader.revision_chunk', params: p }), grant);
  assert.equal((await request(params)).error.code, 'revision_text_unavailable');
  assert.equal(commands.length, 1);
  assert.equal(commands[0].max_sensitivity, 'public');
  for (const bad of [{ ...params, principal: 'local_owner' }, { ...params, max_sensitivity: 'private' },
    { ...params, target: { kind: 'body', side: 'original' } }, { ...params, limit_bytes: 8193 },
    { ...params, start_byte: -1 }, { ...params, expected_sha256: 'invalid' }]) {
    assert((await request(bad)).error);
  }
  assert.equal(commands.length, 1);
});

test('reader admission preserves definite rejection and protects write scope', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'synthetic', generation: 1,
    scopes: ['read', 'edit'], maxSensitivity: 'private', expiresAt: 10000 });
  const request = JSON.stringify({ id: 'test', method: 'memex.library.reader.align_request', params: {
    item_id: 'item_test', revision_id: 'revision_test', chapter: 'chapter-1',
    request_id: 'test', expected_alignment_version: 'missing' } });
  for (const code of ['reader_job_in_progress', 'stale_chapter_alignment', 'no_low_dial_provider',
    'invalid_reader_alignment_request', 'request_identity_conflict', 'internal_error']) {
    const route = createLibraryRouter({ socketPath: '/not-used', authorization,
      sendCommand: async () => ({ ok: false, reason_code: code }) });
    assert.equal((await route(request, grant)).error.code, code === 'internal_error' ? 'unknown' : code);
  }
  const readonly = authorization.set({ sessionId: 'readonly', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async () => { throw new Error('read-only request reached daemon'); } });
  assert.equal((await route(request, readonly)).error.code, 'permission_denied');
});

async function connect(fixture, frames = [], connection = {}) {
  const protocol = await import('../web/protocol.mjs');
  const { openSecureChannel } = await import('../web/app.mjs');
  const pairing = connection.pairing || protocol.parsePairingCode(fixture.pairingCode, fixture.webClientUrl, {
    webClientUrl: fixture.webClientUrl, relayUrl: fixture.relayUrl, allowLocal: true,
  });
  class BrowserSocket extends WebSocket {
    constructor(url, protocols) {
      super(url, protocols, { headers: { origin: fixture.browserOrigin }, perMessageDeflate: false });
      this.on('message', (data) => frames.push(data.toString()));
    }
    send(data, ...args) { frames.push(data.toString()); return super.send(data, ...args); }
  }
  connection.identity ||= await protocol.generateIdentity(webcrypto.subtle);
  return openSecureChannel({
    pairing, identity: connection.identity,
    phoneId: '33333333-3333-4333-8333-333333333333', persistTrust: async (trusted) => { connection.pairing = trusted; },
    WebSocketValue: BrowserSocket, allowLocal: true, timeoutMs: 5000,
  });
}

for (const projection of ['legacy', 'mapped']) {
  test(`translation discovery without provider configuration: ${projection}`, async (t) => {
    const fixture = await startMobileFixture({ translationProjection: projection });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    let n = 0;
    const rpc = (op, params) => channel.request(`discovery_${++n}`, `memex.library.reader.${op}`, params);
    const { item_id, revision_id } = fixture.readerBook;
    const selection = { item_id, revision_id, chapter: 'appendix' };
    const info = (await channel.request('discovery_info', 'memex.session.info', {})).result;
    assert.equal(info.reader_translation_metadata, 'available');
    assert.equal(info.reader_translation_alignment, 'available');
    const counts = async () => (await promisify(execFile)('sqlite3', [path.join(fixture.root, 'agent.db'),
      'SELECT (SELECT count(*) FROM reader_translation_quotes), (SELECT count(*) FROM reader_translation_jobs);'])).stdout;
    const before = await counts();
    assert.equal(before.trim(), '0|0');
    const metadata = (await rpc('translate_metadata', selection)).result.translation;
    assert(metadata?.version);
    const body = (await rpc('translate_chunk', { ...selection, expected_version: metadata.version, offset: 0, limit: 32768 })).result;
    const first = (await rpc('translate_alignment', { ...selection, expected_version: metadata.version, offset: 0, limit: 1 })).result;
    assert.equal(first.basis, 'translation_blocks');
    assert.equal(first.quality, 'not_certified');
    assert.equal(first.translation_chapter, null);
    if (projection === 'legacy') {
      assert.equal(first.status, 'missing');
      assert.equal(first.total, 0);
      assert.deepEqual(first.pairs, []);
    } else {
      assert.equal(first.total, 2);
      assert.equal(first.pairs[0].kind, 'heading');
      const last = (await rpc('translate_alignment', { ...selection, expected_version: metadata.version, offset: first.next_offset, limit: 1 })).result;
      assert.equal(last.next_offset, null);
      assert.equal(last.pairs[0].kind, 'paragraph');
      assert.equal(first.pairs[0].t[1], last.pairs[0].t[0]);
      assert.equal(last.pairs[0].t[1], Buffer.byteLength(body.markdown));
      assert.equal(Buffer.from(body.markdown).subarray(...first.pairs[0].t).toString(), '合成中文译文🙂\n\n');
    }
    assert.equal((await rpc('translate_alignment', { ...selection, expected_version: '0'.repeat(64), offset: 0, limit: 1 })).error.code, 'stale_reader_translation');
    for (const limit of [0, 257]) assert.equal((await rpc('translate_alignment', { ...selection, expected_version: metadata.version, offset: 0, limit })).error.code, 'invalid_reader_translation_request');
    assert.equal((await rpc('translate_metadata', { ...selection, quote_id: 'injected' })).error.code, 'invalid_reader_translation_request');
    assert.equal((await rpc('translate_metadata', selection)).result.translation.version, metadata.version);
    assert.equal(await counts(), before);
  });
}

for (const outcome of ['success', 'failed', 'unknown']) {
  test(`content format durable encrypted gateway: ${outcome}`, async (t) => {
    const fixture = await startMobileFixture({ seedFormat: true, formatOutcome: outcome });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    const info = (await channel.request('format_info', 'memex.session.info', {})).result;
    assert.equal(info.reader_format, 'available');
    assert.equal(info.reader_format_batch, 'available');
    let n = 0;
    const rpc = async (op, params) => {
      for (let i = 0; i < 30; i++) {
        const reply = await channel.request(`format_${++n}`, `memex.library.reader.format_${op}`, params);
        if (reply.error?.code !== 'library_loading') {
          assert(reply.result, JSON.stringify(reply));
          return reply.result;
        }
        assert(!['request', 'restore', 'resume'].includes(op), 'never retry an uncertain write');
        await new Promise(resolve => setTimeout(resolve, 40));
      }
      assert.fail('reader remained busy');
    };
    const selection = { kind: 'item', ...fixture.items[3], target: { kind: 'body' } };
    let preview = await rpc('preview', { selection });
    const deadline = Date.now() + 15000;
    while (preview.state === 'preparing') {
      assert(Date.now() < deadline, 'preview did not settle');
      await new Promise(resolve => setTimeout(resolve, 40));
      preview = await rpc('preview', { preview_id: preview.preview_id });
    }
    assert.equal(preview.state, 'ready', JSON.stringify(preview));
    assert.equal(preview.quote.counts.eligible, 1);
    const input = { quote_id: preview.quote.quote_id, request_id: 'format-explicit', accept_cost: true, accept_model_processing: true };
    let { batch } = await rpc('request', input);
    const batch_id = batch.batch_id;
    while (batch.state !== 'done') {
      assert(Date.now() < deadline, JSON.stringify(batch));
      await new Promise(resolve => setTimeout(resolve, 40));
      ({ batch } = await rpc('status', { batch_id }));
    }
    const expected = outcome === 'success' ? 'succeeded' : outcome === 'unknown' ? 'outcome_unknown' : 'failed';
    assert.equal(batch.counts[expected], 1);
    assert.equal(batch.usage.requests_dispatched, 1);
    // A definite rejection without a usage report still has unknown cost.
    assert.equal(batch.usage.unknown_usage_requests, outcome === 'success' ? 0 : 1);
    const rows = await rpc('results', { batch_id, offset: 0, limit: 100 });
    const row = rows.results[0];
    assert.equal(row.state, expected);
    if (outcome === 'success') {
      assert.equal(row.formatting.changed, true);
      const proof = { batch_id, target_id: row.target_id, expected_version: row.formatting.changes_version };
      const changes = await rpc('changes', { ...proof, view: 'list', offset: 0, limit: 64 });
      assert(changes.total >= 2);
      const deletion = changes.changes.find(change => change.kind === 'garbage_delete');
      assert(deletion);
      const before = await rpc('changes', { ...proof, view: 'text', change_id: deletion.id, part: 'before', start_byte: 0, limit_bytes: 8192 });
      assert.equal(before.text.trim(), 'Next');
      const restore = { batch_id, target_id: row.target_id, expected_revision_id: row.job.result_revision_id, request_id: 'format-restore' };
      const restored = await rpc('restore', restore);
      assert.equal(restored.changed, true);
      assert.notEqual(restored.revision_id, row.revision_id);
      assert.deepEqual(await rpc('restore', restore), restored);
    }
    await fixture.restartDaemon();
    const replay = await rpc('request', input);
    assert.equal(replay.batch.batch_id, batch_id);
    assert.deepEqual(replay.batch.usage, batch.usage);
    assert.equal(replay.batch.state, 'done');
  });
}

test('content format directory encrypted gateway freezes authorized distinct targets', async (t) => {
  const fixture = await startMobileFixture({ seedFormat: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = async (op, params) => {
    const reply = await channel.request(`directory_${++n}`, `memex.library.reader.format_${op}`, params);
    assert(reply.result, JSON.stringify(reply));
    return reply.result;
  };
  const owner = command => sendOwnerCommand(fixture.socket, { max_sensitivity: 'private', ...command });
  const root = await owner({ command: 'group_edit', request_id: 'format-root', expected_version: null,
    ops: [{ op: 'create_root', name: 'Formatting', sensitivity: 'public' }] });
  const group_id = root.group_edit.group_id;
  const added = await owner({ command: 'group_edit', request_id: 'format-members', expected_version: root.group_edit.version,
    ops: [{ op: 'add_members', group_id, item_ids: [fixture.items[0].item_id, fixture.items[2].item_id,
      fixture.items[3].item_id, fixture.paragraphBook.item_id] }] });
  assert(added.ok);
  const selection = { kind: 'group', group_id, include_descendants: false };
  let preview = await rpc('preview', { selection });
  const deadline = Date.now() + 20000;
  while (preview.state === 'preparing') {
    assert(Date.now() < deadline);
    await new Promise(resolve => setTimeout(resolve, 50));
    preview = await rpc('preview', { preview_id: preview.preview_id });
  }
  assert.equal(preview.state, 'ready', JSON.stringify(preview));
  assert.equal(preview.quote.counts.targets, 3, 'hidden member is excluded');
  assert.equal(preview.quote.counts.eligible, 2);
  assert.equal(preview.quote.counts.skipped_by_reason.chapter_required, 1);
  const input = { quote_id: preview.quote.quote_id, request_id: 'directory-batch', accept_cost: true, accept_model_processing: true };
  let { batch } = await rpc('request', input);
  while (batch.state !== 'done') {
    assert(Date.now() < deadline, JSON.stringify(batch));
    await new Promise(resolve => setTimeout(resolve, 50));
    ({ batch } = await rpc('status', { batch_id: batch.batch_id }));
  }
  assert.equal(batch.counts.succeeded, 2);
  assert.equal(batch.counts.skipped, 1);
  assert.equal(batch.usage.requests_dispatched, preview.quote.budget.max_requests);
  const first = await rpc('results', { batch_id: batch.batch_id, offset: 0, limit: 1 });
  assert.equal(first.total, 3);
  assert.equal(first.next_offset, 1);
  const rest = await rpc('results', { batch_id: batch.batch_id, offset: first.next_offset, limit: 100 });
  assert.equal(new Set([...first.results, ...rest.results].map(r => r.target_id)).size, 3);
  assert(![...first.results, ...rest.results].some(r => r.item_id === fixture.items[2].item_id));
  let cached = await rpc('preview', { selection });
  while (cached.state === 'preparing') {
    assert(Date.now() < deadline);
    await new Promise(resolve => setTimeout(resolve, 50));
    cached = await rpc('preview', { preview_id: cached.preview_id });
  }
  assert.equal(cached.quote.budget.max_requests, 0);
  assert.equal(cached.quote.counts.skipped_by_reason.already_formatted, 2);
  // A directory removal is not global trash. Install the real Store marker in
  // this isolated synthetic fixture to exercise actual resource unavailability.
  const hidden = [...first.results, ...rest.results].find(r => r.item_id === fixture.items[0].item_id);
  const marker = path.join(fixture.root, 'Library/catalog/trash', `${hidden.item_id}.json`);
  fs.mkdirSync(path.dirname(marker), { recursive: true });
  fs.writeFileSync(marker, JSON.stringify({ schema_version: 1, item_id: hidden.item_id,
    revision_id: hidden.job.result_revision_id, trashed_at: new Date().toISOString(),
    reason: 'synthetic', kept_item_id: null, plan_id: null }));
  for (const [op, params] of [['status', { batch_id: batch.batch_id }],
    ['results', { batch_id: batch.batch_id, offset: 0, limit: 100 }],
    ['request', input], ['resume', { batch_id: batch.batch_id }]]) {
    const denied = await channel.request(`denied_${++n}`, `memex.library.reader.format_${op}`, params);
    assert(denied.error, JSON.stringify(denied));
    assert.equal(denied.result, undefined, 'no batch identities, counts or usage on denial');
  }
  const hiddenProof = await channel.request('hidden_proof', 'memex.library.reader.format_changes', {
    batch_id: batch.batch_id, target_id: hidden.target_id, expected_version: hidden.formatting.changes_version,
    view: 'list', offset: 0, limit: 64,
  });
  assert(hiddenProof.error);
  assert.equal(hiddenProof.result, undefined);
  const retained = [...first.results, ...rest.results].find(r => r.item_id === fixture.items[3].item_id);
  const readable = await rpc('changes', { batch_id: batch.batch_id, target_id: retained.target_id,
    expected_version: retained.formatting.changes_version, view: 'list', offset: 0, limit: 64 });
  assert(readable.total > 0, 'unrelated trash must not hide authorized proof');
  const restore = { batch_id: batch.batch_id, target_id: retained.target_id,
    expected_revision_id: retained.job.result_revision_id, request_id: 'format-restore-retained' };
  const restored = await rpc('restore', restore);
  assert.equal(restored.changed, true);
  assert.deepEqual(await rpc('restore', restore), restored);
  // Explicit synthetic owner action restores B, never an automatic formatter bypass.
  const untrashed = await sendOwnerCommand(fixture.socket, { command: 'item_trash_restore', item_id: hidden.item_id });
  assert(untrashed.ok, JSON.stringify(untrashed));
  const resumed = await rpc('resume', { batch_id: batch.batch_id });
  assert.equal(resumed.batch.batch_id, batch.batch_id);
  assert.deepEqual(resumed.batch.usage, batch.usage);
});

for (const outcome of ['success', 'failed', 'unknown']) {
  test(`selected chapter job over encrypted gateway: ${outcome}`, async (t) => {
    const fixture = await startMobileFixture({ seedRepair: true, repairOutcome: outcome });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    let n = 0;
    const rpc = (method, params) => channel.request(`job_${++n}`, `memex.${method}`, params);
    assert.equal((await rpc('session.info', {})).result.reader_repair, 'available');
    const { item_id, revision_id } = fixture.readerBook;
    const input = { item_id, revision_id, chapter: 'chapter-1', request_id: 'synthetic-explicit-job', expected_alignment_version: 'missing' };
    const admitted = await rpc('library.reader.align_request', input);
    assert(admitted.result, JSON.stringify(admitted));
    let job = admitted.result.job;
    const deadline = Date.now() + 10000;
    while (['queued', 'running'].includes(job.state)) {
      assert(Date.now() < deadline, 'synthetic job did not finish');
      await new Promise((resolve) => setTimeout(resolve, 40));
      const response = await rpc('library.reader.align_status', { item_id, job_id: job.job_id });
      if (response.error?.code === 'library_loading') continue;
      assert(response.result, JSON.stringify(response));
      job = response.result.job;
    }
    assert.equal(job.state, outcome === 'success' ? 'succeeded' : outcome === 'unknown' ? 'outcome_unknown' : 'failed');
    assert.equal(job.usage.requests_dispatched, 1);
    assert.equal(job.usage.reported_total_tokens, outcome === 'success' ? 37 : outcome === 'failed' ? 17 : 0);
    assert.equal(job.usage.unknown_usage_requests, outcome === 'unknown' ? 1 : 0);
    assert.equal(job.operation, 'repair');
    assert.deepEqual(job.usage.dispatches, [{ input_tokens: null, output_tokens: null,
      total_tokens: outcome === 'success' ? 37 : outcome === 'failed' ? 17 : null }]);
    assert.equal(job.usage.input_tokens, null);
    assert.equal(job.usage.output_tokens, null);
    assert.equal(job.failure_stage, outcome === 'success' ? null : 'matching');
    const aligned = (await rpc('library.reader.alignment', { item_id, revision_id, chapter: 'chapter-1', offset: 0, limit: 128, expected_version: 'any' })).result;
    assert.equal(aligned.total, outcome === 'success' ? 6 : 0);
    await fixture.restartDaemon();
    const replay = (await rpc('library.reader.align_request', input)).result.job;
    assert.equal(replay.job_id, job.job_id);
    assert.deepEqual(replay.usage, job.usage);
    assert.equal(replay.state, job.state);
  });
}

for (const outcome of ['success', 'failed', 'unknown']) {
  test(`chapter translation over encrypted gateway: ${outcome}`, async (t) => {
    const fixture = await startMobileFixture({ seedTranslation: true, translationOutcome: outcome });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    let n = 0;
    const rpc = (op, params) => channel.request(`translation_${++n}`, `memex.library.reader.${op}`, params);
    assert.equal((await channel.request('translation_info', 'memex.session.info', {})).result.reader_translation, 'available');
    const { item_id, revision_id } = fixture.readerBook;
    const selection = { item_id, revision_id, chapter: 'appendix' };
    assert.equal((await rpc('translate_metadata', selection)).result.translation, null);
    const paired = await rpc('translate_preview', { ...selection, chapter: 'chapter-1' });
    assert.equal(paired.result.eligible, false);
    const preview = await rpc('translate_preview', selection);
    assert(preview.result?.quote, JSON.stringify(preview));
    const input = { ...selection, request_id: 'translation-explicit', quote_id: preview.result.quote.quote_id,
      accept_cost: true, expected_translation_version: 'missing' };
    const admission = await rpc('translate_request', input);
    assert(admission.result?.job, JSON.stringify(admission));
    let job = admission.result.job;
    const deadline = Date.now() + 10000;
    while (['queued', 'running'].includes(job.state)) {
      assert(Date.now() < deadline, 'translation did not finish');
      await new Promise((r) => setTimeout(r, 40));
      const response = await rpc('translate_status', { item_id, job_id: job.job_id });
      if (response.error?.code === 'library_loading') continue;
      assert(response.result?.job, JSON.stringify(response));
      job = response.result.job;
    }
    assert.equal(job.state, outcome === 'success' ? 'succeeded' : outcome === 'unknown' ? 'outcome_unknown' : 'failed');
    assert.equal(job.usage.requests_dispatched, 1);
    assert.deepEqual(job.usage.dispatches, [{ input_tokens: outcome === 'success' ? 11 : null,
      output_tokens: outcome === 'success' ? 3 : null, total_tokens: outcome === 'success' ? 17 : null }]);
    assert.equal((await rpc('translate_request', input)).result.job.job_id, job.job_id);
    const toc = await rpc('toc', { item_id, revision_id });
    assert.equal(toc.result.revision_id, revision_id);
    assert.equal(toc.result.editions[0].chapters.find((c) => c.key === 'appendix').counterpart, null);
    if (outcome === 'success') {
      const discovered = await rpc('translate_metadata', selection);
      assert.equal(discovered.result.translation.version, job.translation.version);
      const alignment = await rpc('translate_alignment', { ...selection, expected_version: discovered.result.translation.version, offset: 0, limit: 256 });
      assert.equal(alignment.result.total, 2);
      assert(alignment.result.pairs.every((pair) => pair.relation === 'matched'));
      const chunk = await rpc('translate_chunk', { ...selection, expected_version: job.translation.version, offset: 0, limit: 32768 });
      assert(chunk.result?.markdown.includes('合成中文译文'), JSON.stringify(chunk));
      const cached = await rpc('translate_preview', selection);
      assert.equal(cached.result.translation.version, job.translation.version);
      const reused = await rpc('translate_request', { ...input, request_id: 'translation-cache' });
      assert.equal(reused.result.job.usage.requests_dispatched, 0);
    } else {
      assert.equal(job.translation, null);
    }
  });
}

test('chapter edits use authenticated staging, restart replay and immutable selected-side revisions', async (t) => {
  const fixture = await startMobileFixture({ seedParagraphs: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (op, params) => channel.request(`edit_${++n}`, `memex.library.reader.${op}`, params);
  const info = await channel.request('edit_info', 'memex.session.info', {});
  assert.equal(info.result.reader_edit, 'available');
  const { item_id, revision_id } = fixture.readerBook;
  const initial = { item_id, revision_id };
  const old = (await rpc('chunk', { ...initial, side: 'original', chapter: 'chapter-1', offset: 0, limit: 32768 })).result;
  const translated = (await rpc('chunk', { ...initial, side: 'translation', chapter: 'chapter-1', offset: 0, limit: 32768 })).result;
  const hash = (text) => createHash('sha256').update(text).digest('hex');
  const markdown = '# Changed owner heading\n\nEdited prose 🌹.\n\n';
  const input = { ...initial, side: 'original', chapter: 'chapter-1', request_id: 'editor-1',
    expected_chapter_sha256: hash(old.markdown), bytes: Buffer.byteLength(markdown), sha256: hash(markdown) };
  const upload = await rpc('edit_begin', input);
  assert(upload.result, JSON.stringify(upload));
  const { edit_id } = upload.result;
  assert.equal((await rpc('edit_begin', { ...input, principal: 'local_owner' })).error.code, 'invalid_reader_edit');
  assert.equal((await rpc('edit_chunk', { item_id, edit_id, offset: 1, markdown: 'gap' })).error.code, 'reader_edit_upload_conflict');
  assert.equal((await rpc('edit_chunk', { item_id, edit_id, offset: 0, markdown: 'x'.repeat(8193) })).error.code, 'invalid_reader_edit');
  const first = markdown.slice(0, 25);
  assert.equal((await rpc('edit_chunk', { item_id, edit_id, offset: 0, markdown: first })).result.received_bytes, Buffer.byteLength(first));
  await fixture.restartDaemon();
  assert.equal((await rpc('edit_begin', input)).result.received_bytes, Buffer.byteLength(first));
  assert.equal((await rpc('edit_chunk', { item_id, edit_id, offset: 0, markdown: first })).result.received_bytes, Buffer.byteLength(first));
  assert.equal((await rpc('edit_chunk', { item_id, edit_id, offset: Buffer.byteLength(first), markdown: markdown.slice(25) })).result.received_bytes, input.bytes);
  const foreign = await sendOwnerCommand(fixture.socket, { command: 'reader', principal: 'a'.repeat(64),
    max_sensitivity: 'public', request: { op: 'edit_commit', item_id, edit_id } });
  assert.equal(foreign.reason_code, 'reader_edit_not_found');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  assert.equal((await rpc('edit_commit', { item_id, edit_id })).error.code, 'permission_denied');
  fixture.authorization.set(fixture.grant);
  const saved = await rpc('edit_commit', { item_id, edit_id });
  assert(saved.result, JSON.stringify(saved));
  assert.equal(saved.result.changed, true);
  assert.notEqual(saved.result.revision_id, revision_id);
  assert.equal(saved.result.chapter_sha256, input.sha256);
  await fixture.restartDaemon();
  assert.deepEqual((await rpc('edit_commit', { item_id, edit_id })).result, saved.result);
  assert.deepEqual((await rpc('edit_begin', input)).result.receipt, saved.result);
  const current = { item_id, revision_id: saved.result.revision_id };
  assert.equal((await rpc('chunk', { ...current, side: 'original', chapter: 'chapter-1', offset: 0, limit: 32768 })).result.markdown, markdown);
  assert.equal((await rpc('chunk', { ...current, side: 'translation', chapter: 'chapter-1', offset: 0, limit: 32768 })).result.markdown, translated.markdown);
  assert.equal((await rpc('alignment', { ...current, chapter: 'chapter-1', offset: 0, limit: 128, expected_version: 'any' })).result.version, 'missing');
  assert.equal((await rpc('edit_begin', { ...input, request_id: 'stale-editor' })).error.code, 'stale_item_revision');
});

// This is the real fs2/flock writer lock, not a delayed response adapter. The
// helper announces ownership; lsof then observes the daemon opening that same
// inode while the exclusive holder remains alive. No sleep stands in for arrival.
async function chapterWriterBarrier(t, fixture) {
  const lock = path.join(fixture.root, 'Library/catalog/.ingest.lock');
  const child = spawn('python3', ['-u', '-c',
    'import fcntl,sys\nf=open(sys.argv[1],"r+")\nfcntl.flock(f,fcntl.LOCK_EX)\nprint("ready",flush=True)\nsys.stdin.readline()\n', lock],
  { cwd: path.resolve(__dirname, '../..'), stdio: ['pipe', 'pipe', 'pipe'] });
  const exited = once(child, 'exit');
  let released = false;
  const release = async () => {
    if (!released) { released = true; child.stdin.end('\n'); }
    await exited;
  };
  t.after(release);
  assert.equal((await once(child.stdout, 'data', { signal: AbortSignal.timeout(3000) }))[0].toString().trim(), 'ready');
  return { release, async waitForDaemon() {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      try {
        const { stdout } = await promisify(execFile)('/usr/sbin/lsof',
          ['-a', '-p', String(fixture.pid), '-F', 'pf', lock], { timeout: 1000 });
        if (stdout.includes(`p${fixture.pid}\n`) && /^f\d+/m.test(stdout)) return;
      } catch (error) { if (error.code !== 1) throw error; }
    }
    assert.fail('daemon did not open the held Store writer lock');
  } };
}

async function chapterUpload(channel, fixture) {
  let n = 0;
  const rpc = (op, params) => channel.request(`boundary_${++n}`, `memex.library.reader.${op}`, params);
  const { item_id, revision_id } = fixture.readerBook;
  const old = await rpc('chunk', { item_id, revision_id, side: 'original', chapter: 'chapter-1', offset: 0, limit: 32768 });
  assert(old.result);
  const hash = (text) => createHash('sha256').update(text).digest('hex');
  const markdown = '# Synthetic authorized edit\n\nA changed chapter.\n';
  const input = { item_id, revision_id, side: 'original', chapter: 'chapter-1',
    request_id: 'authorization-boundary', expected_chapter_sha256: hash(old.result.markdown),
    bytes: Buffer.byteLength(markdown), sha256: hash(markdown) };
  return { rpc, input, async upload() {
    const begun = await rpc('edit_begin', input);
    assert(begun.result);
    const commit = { item_id, edit_id: begun.result.edit_id };
    assert.equal((await rpc('edit_chunk', { ...commit, offset: 0, markdown })).result.received_bytes, input.bytes);
    return commit;
  } };
}

for (const change of ['revoke', 'expire']) {
  for (const phase of ['before-begin', 'after-upload', 'writer-wait']) {
    test(`chapter edit auth boundary: ${change} ${phase}`, { timeout: 20000 }, async (t) => {
      let now = Date.now();
      const commands = [];
      let entered;
      let completed;
      const dispatched = new Promise((resolve) => { entered = resolve; });
      const ownerResult = new Promise((resolve) => { completed = resolve; });
      const fixture = await startMobileFixture({ seedParagraphs: true, now: () => now,
        sendCommand: (socket, command, options) => {
          commands.push(command);
          const result = sendOwnerCommand(socket, command, options);
          if (command.request?.op === 'edit_commit') {
            entered();
            result.then(completed, () => completed(null));
          }
          return result;
        } });
      t.after(() => fixture.close());
      const frames = [];
      const connection = {};
      const channel = await connect(fixture, frames, connection);
      t.after(() => channel.close());
      const stage = await chapterUpload(channel, fixture);
      const commit = phase === 'before-begin' ? null : await stage.upload();
      const revisions = path.join(fixture.root, 'Library/items', stage.input.item_id, 'revisions');
      const before = fs.readdirSync(revisions).sort();
      const revoke = () => {
        if (change === 'expire') now = fixture.grant.expiresAt;
        else fixture.authorization.revoke(fixture.grant.sessionId);
      };
      if (phase !== 'writer-wait') {
        const count = commands.length;
        revoke();
        await assert.rejects(stage.rpc(commit ? 'edit_commit' : 'edit_begin', commit || stage.input));
        assert.equal(commands.length, count, 'denied request must start zero owner commands');
        assert.deepEqual(fs.readdirSync(revisions).sort(), before);
        return;
      }
      const barrier = await chapterWriterBarrier(t, fixture);
      const rejected = assert.rejects(stage.rpc('edit_commit', commit));
      await dispatched;
      await barrier.waitForDaemon();
      assert.deepEqual(fs.readdirSync(revisions).sort(), before, 'no mutation while lock is held');
      const sent = frames.length;
      revoke();
      await barrier.release();
      const saved = await ownerResult;
      assert.equal(saved?.ok, true);
      assert.equal(saved.reader.changed, true);
      await rejected;
      assert.equal(frames.length, sent, 'old encrypted channel must not deliver the receipt');
      const after = fs.readdirSync(revisions).sort();
      assert.equal(after.length, before.length + 1);
      channel.close();
      fixture.authorization.set({ ...fixture.grant, expiresAt: now + 60000 });
      await fixture.restartBridge();
      const fresh = await connect(fixture, [], connection);
      t.after(() => fresh.close());
      const replay = await fresh.request('fresh_commit', 'memex.library.reader.edit_commit', commit);
      assert.deepEqual(replay.result, saved.reader);
      const begin = await fresh.request('fresh_begin', 'memex.library.reader.edit_begin', stage.input);
      assert.deepEqual(begin.result.receipt, saved.reader);
      assert.deepEqual(fs.readdirSync(revisions).sort(), after, 'recovery cannot generate a second revision');
    });
  }
}

for (const phase of ['mutation', 'replay']) {
  test(`chapter edit auth boundary: trash denies ${phase} under writer lock`, { timeout: 15000 }, async (t) => {
    const fixture = await startMobileFixture({ seedParagraphs: true });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    const stage = await chapterUpload(channel, fixture);
    const commit = await stage.upload();
    let revision = stage.input.revision_id;
    if (phase === 'replay') {
      const saved = await stage.rpc('edit_commit', commit);
      assert(saved.result);
      revision = saved.result.revision_id;
    }
    const revisions = path.join(fixture.root, 'Library/items', commit.item_id, 'revisions');
    const before = fs.readdirSync(revisions).sort();
    const barrier = await chapterWriterBarrier(t, fixture);
    const pending = stage.rpc('edit_commit', commit);
    await barrier.waitForDaemon();
    // Synthetic fixture mutation while holding the actual Store lock: the same
    // recoverable marker shape used by trash.rs, with no product test hook.
    const dir = path.join(fixture.root, 'Library/catalog/trash');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, `${commit.item_id}.json`), JSON.stringify({
      schema_version: 1, item_id: commit.item_id, revision_id: revision,
      trashed_at: '2026-01-01T00:00:00Z', reason: 'synthetic_test', kept_item_id: null, plan_id: null,
    }));
    await barrier.release();
    assert.equal((await pending).error?.code, 'library_resource_not_found');
    assert.equal((await stage.rpc('edit_begin', stage.input)).error?.code, 'library_resource_not_found');
    assert.deepEqual(fs.readdirSync(revisions).sort(), before);
  });
}

test('chapter edit auth boundary: upload does not bypass current revision CAS', async (t) => {
  const fixture = await startMobileFixture({ seedParagraphs: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const stage = await chapterUpload(channel, fixture);
  const commit = await stage.upload();
  const changed = await channel.request('intervening_metadata', 'memex.library.metadata.update', {
    item_id: commit.item_id, expected_revision_id: stage.input.revision_id,
    title: 'Synthetic intervening title', manual_tags: ['synthetic'],
  });
  assert(changed.result);
  const revisions = path.join(fixture.root, 'Library/items', commit.item_id, 'revisions');
  const before = fs.readdirSync(revisions).sort();
  const barrier = await chapterWriterBarrier(t, fixture);
  const pending = stage.rpc('edit_commit', commit);
  await barrier.waitForDaemon();
  await barrier.release();
  assert.equal((await pending).error?.code, 'stale_item_revision');
  assert.deepEqual(fs.readdirSync(revisions).sort(), before);
});

test('historical revision text over encrypted gateway preserves exact bytes and current privacy', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`history_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.reader_revision_text, 'available');
  const entry = (await rpc('library.item.get', { item_id: fixture.items[0].item_id })).result;
  const read = { item_id: entry.item_id, revision_id: entry.revision_id, target: { kind: 'body' },
    start_byte: 0, limit_bytes: 8192, expected_sha256: 'any' };
  const updated = await rpc('library.metadata.update', { item_id: entry.item_id,
    expected_revision_id: entry.revision_id, title: 'History fixture revised', manual_tags: [] });
  assert(updated.result);
  assert.notEqual(updated.result.revision_id, entry.revision_id);
  let text = '', sha;
  for (;;) {
    const response = await rpc('library.reader.revision_chunk', read);
    assert(response.result, JSON.stringify(response));
    const part = response.result;
    sha ||= part.sha256;
    assert.equal(part.sha256, sha);
    assert.equal(part.revision_id, entry.revision_id);
    assert(Buffer.byteLength(part.text) <= 8192);
    text += part.text;
    if (part.next_offset === null) break;
    assert(part.next_offset > read.start_byte);
    read.start_byte = part.next_offset;
    read.expected_sha256 = sha;
  }
  assert.equal(text, `A${'x'.repeat(16382)}界🙂Z`);
  assert.equal(sha, createHash('sha256').update(text).digest('hex'));
  assert.equal((await rpc('library.reader.toc', { item_id: entry.item_id,
    revision_id: entry.revision_id })).error.code, 'stale_item_revision');
  assert((await rpc('library.reader.revision_chunk', { ...read, expected_sha256: '0'.repeat(64) })).error);
  const hidden = await rpc('library.reader.revision_chunk', { ...read, ...fixture.items[2], start_byte: 0, expected_sha256: 'any' });
  assert.equal(hidden.error.code, 'library_resource_not_found');
  await fixture.restartDaemon();
  const replay = await rpc('library.reader.revision_chunk', { ...read, start_byte: 16383 });
  assert.equal(replay.result?.text, '界🙂Z');
});

test('encrypted mobile gateway uses real Store pagination, UTF8 slices and scoped CAS', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`rpc_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.generation_ready, false);
  const first = (await rpc('library.query', window)).result;
  assert.equal(first.total, 2);
  assert.equal(first.next_offset, 1);
  assert.equal(first.items[0].title, 'Amber public');
  assert.deepEqual(first.sources, [{ value: 'https://synthetic.example.invalid', count: 2 }]);
  assert.deepEqual(first.item_types, [{ value: 'webpage', count: 2 }]);
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'private' });
  assert((await rpc('library.metadata.update', {
    item_id: fixture.items[2].item_id, expected_revision_id: fixture.items[2].revision_id,
    title: 'Crimson private revised', manual_tags: ['hidden'],
  })).result);
  fixture.authorization.set(fixture.grant);
  const unchanged = (await rpc('library.query', window)).result;
  assert.equal(unchanged.generation, first.generation, 'hidden-only changes must not leak through tokens');
  assert.equal(unchanged.view, first.view);
  const second = (await rpc('library.query', { ...window, offset: 1, expected_view: first.view })).result;
  assert.equal(second.items[0].title, 'Zinc public');
  assert.equal(second.next_offset, null);
  assert.equal((await rpc('library.query', { ...window, max_sensitivity: 'restricted' })).error.code, 'invalid_request');
  for (const method of ['library.item.get', 'item.read', 'capture.enqueue']) {
    const response = await rpc(method, { item_id: fixture.items[2].item_id });
    assert.equal(response.result, undefined);
  }
  assert.equal((await rpc('library.context.read', {
    ...fixture.items[2], expected_revision_id: 'revision_wrong', revision_id: undefined,
    start_byte: 0, end_byte: 4,
  })).error.code, 'context_resource_not_found');
  const hiddenEdit = await rpc('library.metadata.update', {
    item_id: fixture.items[2].item_id, expected_revision_id: 'revision_wrong',
    title: 'Do not write', manual_tags: [],
  });
  assert.equal(hiddenEdit.error.code, 'library_resource_not_found');
  const entry = (await rpc('library.item.get', { item_id: fixture.items[0].item_id })).result;
  assert.equal(entry.next_byte, 16383);
  assert.equal(entry.total_bytes, 16391);
  assert.equal(entry.markdown, `A${'x'.repeat(16382)}`);
  const params = { item_id: entry.item_id, expected_revision_id: entry.revision_id,
    start_byte: entry.next_byte, end_byte: entry.total_bytes };
  const tail = (await rpc('library.context.read', params)).result;
  assert.equal(tail.content_markdown, '界🙂Z');
  assert.equal(tail.content_sha256, entry.content_sha256);
  assert.equal(entry.content_sha256, `sha256:${createHash('sha256').update(entry.markdown + '界🙂Z').digest('hex')}`);
  assert.equal((await rpc('library.context.read', { ...params, end_byte: 16384 })).error.code, 'invalid_context_range');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const update = { item_id: entry.item_id, expected_revision_id: entry.revision_id,
    title: 'Amber edited once', manual_tags: ['zeta', 'alpha'] };
  assert.equal((await rpc('library.metadata.update', update)).error.code, 'permission_denied');
  assert.equal((await rpc('library.item.get', { item_id: entry.item_id })).result.title, 'Amber public');
  fixture.authorization.set(fixture.grant);
  const receipt = (await rpc('library.metadata.update', update)).result;
  assert.notEqual(receipt.revision_id, entry.revision_id);
  assert.equal(receipt.previous_revision_id, entry.revision_id);
  assert.equal((await rpc('library.metadata.update', { ...update, title: 'Lost update' })).error.code, 'stale_item_revision');
  const edited = (await rpc('library.item.get', { item_id: entry.item_id })).result;
  assert.equal(edited.title, 'Amber edited once');
  assert.deepEqual(edited.manual_tags, ['alpha', 'zeta']);
  assert.equal((await rpc('library.context.read', params)).error.code, 'stale_item_revision');
  assert.equal((await rpc('library.query', { ...window, offset: 1, expected_view: first.view })).error.code, 'stale_query_view');
  assert.equal((await rpc('library.query', { ...window, query: { ...query, text: 'private_canary' } })).result.total, 0);
  // Same transport, larger server grant: hidden item really exists, not a fake 404.
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'private' });
  assert.equal((await rpc('library.query', { ...window, limit: 200 })).result.total, 4);
  assert.equal((await rpc('library.item.get', { item_id: fixture.items[2].item_id })).result.title, 'Crimson private revised');
  assert.equal((await rpc('library.query', { ...window, expected_view: first.view, offset: 1 })).error.code, 'stale_query_view');
  assert(!frames.join('\n').includes('Amber edited once'));
  assert(!frames.join('\n').includes('PRIVATE_CANARY_NOT_FOR_PUBLIC'));
  assert(frames.some((frame) => JSON.parse(frame).kind === 'encrypted_envelope'));
});

test('unpaired admitted socket cannot read application data', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const payload = JSON.parse(Buffer.from(fixture.pairingCode.split(':')[1], 'base64url'));
  const socket = new WebSocket(`${fixture.relayUrl}/${payload.session_id}`, clientProtocols(
    'iphone', 'browser', payload.iphone_admission_capability,
  ), { headers: { origin: fixture.browserOrigin } });
  t.after(() => socket.terminate());
  const frames = [];
  socket.on('message', (data) => frames.push(data.toString()));
  await once(socket, 'open');
  const closed = once(socket, 'close');
  socket.send(JSON.stringify({ id: 'raw', method: 'memex.library.query', params: window }));
  await closed;
  assert.deepEqual(frames, []);
});

for (const change of ['revoke', 'expire', 'scope', 'ceiling', 'trust']) {
  test(`${change} during a real Store read suppresses the in-flight plaintext result`, async (t) => {
    let now = Date.now();
    let release;
    const gate = new Promise((resolve) => { release = resolve; });
    let entered;
    const dispatched = new Promise((resolve) => { entered = resolve; });
    const fixture = await startMobileFixture({ now: () => now,
      sendCommand: async (...args) => {
        const result = await sendOwnerCommand(...args);
        entered();
        await gate;
        return result;
      },
    });
    t.after(() => fixture.close());
    t.after(() => release());
    if (change === 'ceiling') fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'private' });
    const frames = [];
    const channel = await connect(fixture, frames);
    t.after(() => channel.close());
    const result = change === 'ceiling'
      ? channel.request('pending', 'memex.library.item.get', { item_id: fixture.items[2].item_id })
      : channel.request('pending', 'memex.library.query', window);
    const rejected = assert.rejects(result);
    await dispatched;
    const sentBeforeChange = frames.length;
    if (change === 'revoke') fixture.authorization.revoke(fixture.grant.sessionId);
    if (change === 'expire') now = fixture.grant.expiresAt;
    if (change === 'scope') fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
    if (change === 'ceiling') fixture.authorization.set(fixture.grant);
    if (change === 'trust') fixture.stateStore.update(null, (state) => ({ ...state, trusted_phone: null }));
    release();
    await rejected;
    assert.equal(frames.length, sentBeforeChange, 'no application response may leave after authority changes');
  });
}

test('revoked in-flight edit is unknown to phone, not a promise of Store rollback', async (t) => {
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  let committed;
  const committedWrite = new Promise((resolve) => { committed = resolve; });
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    const response = await sendOwnerCommand(...args);
    if (args[1].command === 'scoped_metadata_update') {
      assert.equal(response.ok, true);
      committed();
      await gate;
    }
    return response;
  } });
  t.after(() => fixture.close());
  t.after(() => release());
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  const item = fixture.items[0];
  const rejected = assert.rejects(channel.request('write', 'memex.library.metadata.update', {
    item_id: item.item_id, expected_revision_id: item.revision_id,
    title: 'Committed before revoke', manual_tags: ['confirmed'],
  }));
  await committedWrite;
  const sent = frames.length;
  fixture.authorization.revoke(fixture.grant.sessionId);
  release();
  await rejected;
  assert.equal(frames.length, sent);
  const readback = await sendOwnerCommand(fixture.socket, { command: 'scoped_item_read',
    item_id: item.item_id, max_sensitivity: 'public' });
  assert.equal(readback.library_entry.title, 'Committed before revoke');
  assert.notEqual(readback.library_entry.revision_id, item.revision_id);
});

test('lost daemon receipt returns unknown over the still-connected encrypted channel', async (t) => {
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    const response = await sendOwnerCommand(...args);
    if (args[1].command === 'scoped_metadata_update') throw new Error('daemon_unavailable');
    return response;
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const item = fixture.items[0];
  const response = await channel.request('lost', 'memex.library.metadata.update', {
    item_id: item.item_id, expected_revision_id: item.revision_id,
    title: 'Durable lost receipt', manual_tags: [],
  });
  assert.equal(response.error.code, 'unknown');
  const readback = await channel.request('readback', 'memex.library.item.get', { item_id: item.item_id });
  assert.equal(readback.result.title, 'Durable lost receipt');
  assert.notEqual(readback.result.revision_id, item.revision_id);
});

function gatedFixture(command, extra = {}) {
  let release;
  let entered;
  const gate = new Promise((resolve) => { release = resolve; });
  const reached = new Promise((resolve) => { entered = resolve; });
  return {
    release: () => release(),
    reached,
    start: () => startMobileFixture({ ...extra, sendCommand: async (...args) => {
      if (args[1].command === command) { entered(); await gate; }
      return sendOwnerCommand(...args);
    } }),
  };
}

test('a slow owner call does not hold back later replies, and each reply keeps its request id', async (t) => {
  const gated = gatedFixture('scoped_item_read');
  const fixture = await gated.start();
  t.after(() => fixture.close());
  t.after(() => gated.release());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const slow = channel.request('slow_read', 'memex.library.item.get', { item_id: fixture.items[0].item_id });
  await gated.reached;
  const fast = await channel.request('fast_query', 'memex.library.query', window);
  assert.equal(fast.id, 'fast_query');
  assert.equal(fast.result.total, 2);
  let slowDone = false;
  slow.then(() => { slowDone = true; });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(slowDone, false, 'the gated read must still be pending after the later reply');
  gated.release();
  const late = await slow;
  assert.equal(late.id, 'slow_read');
  assert.equal(late.result.item_id, fixture.items[0].item_id, JSON.stringify(late.error));
});

test('requests started together each get their own counter and their own reply', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const replies = await Promise.all([
    channel.request('together_1', 'memex.library.query', window),
    channel.request('together_2', 'memex.library.item.get', { item_id: fixture.items[0].item_id }),
    channel.request('together_3', 'memex.session.info', {}),
  ]);
  assert.deepEqual(replies.map((r) => r.id), ['together_1', 'together_2', 'together_3']);
  assert.equal(replies[0].result.total, 2);
  assert.equal(replies[1].result.item_id, fixture.items[0].item_id);
  assert.equal(replies[2].result.generation_ready, false);
});

test('a reply that finishes after revocation is never sent', async (t) => {
  const gated = gatedFixture('scoped_item_read');
  const fixture = await gated.start();
  t.after(() => fixture.close());
  t.after(() => gated.release());
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  const slow = assert.rejects(channel.request('slow_read', 'memex.library.item.get', { item_id: fixture.items[0].item_id }));
  await gated.reached;
  const sent = frames.length;
  fixture.authorization.revoke(fixture.grant.sessionId);
  gated.release();
  await slow;
  assert.equal(frames.length, sent, 'no frame may leave the Mac after revocation');
});

test('item chat reaches the daemon with its own long deadline and returns the reply', async (t) => {
  const seen = [];
  const fixture = await startMobileFixture({ sendCommand: async (socket, command, options) => {
    if (command.command !== 'item_chat') return sendOwnerCommand(socket, command, options);
    seen.push({ command, options });
    return { ok: true, item_chat: { request_id: command.request_id, reply: 'synthetic reply', model: 'fake', finish: 'stop' } };
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const messages = [{ role: 'user', content: '这篇讲了什么？' }];
  const response = await channel.request('chat_1', 'memex.library.item.chat', {
    item_id: fixture.items[0].item_id, request_id: 'mchat_0123456789abcdef', messages,
  });
  assert.equal(response.result.reply, 'synthetic reply');
  assert.equal(seen.length, 1);
  assert.deepEqual(seen[0].command, { command: 'item_chat', item_id: fixture.items[0].item_id,
    max_sensitivity: fixture.grant.maxSensitivity, request_id: 'mchat_0123456789abcdef', messages });
  assert(seen[0].options.timeoutMs > 120_000);
  const other = await channel.request('q', 'memex.library.query', window);
  assert.equal(other.result.total, 2, 'ordinary reads keep the default deadline path');
  const bad = await channel.request('chat_bad', 'memex.library.item.chat', {
    item_id: fixture.items[0].item_id, request_id: 'short', messages,
  });
  assert.equal(bad.error.code, 'invalid_request');
  assert.equal(seen.length, 1);
});

for (const change of ['expire', 'revoke']) {
  test(`${change} before dispatch cannot read even with paired identity`, async (t) => {
    let now = Date.now();
    const fixture = await startMobileFixture({ now: () => now });
    t.after(() => fixture.close());
    const channel = await connect(fixture);
    t.after(() => channel.close());
    if (change === 'expire') now = fixture.grant.expiresAt;
    else fixture.authorization.revoke(fixture.grant.sessionId);
    await assert.rejects(channel.request('denied', 'memex.library.item.get', { item_id: fixture.items[0].item_id }));
  });
}

async function untilResult(operation, expectedState) {
  for (let i = 0; i < 100; i += 1) {
    const response = await operation();
    assert(!response.error, JSON.stringify(response.error));
    if (response.result.state === expectedState) return response.result;
    assert(!['failed', 'needs_user', 'unavailable'].includes(response.result.state), JSON.stringify(response.result));
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  assert.fail(`did not reach ${expectedState}`);
}

test('scoped mobile capture persists once, returns validated segmented translation and rejects stale/hidden reads', async (t) => {
  let loseReceipt = true;
  const fixture = await startMobileFixture({ capture: true, sendCommand: async (...args) => {
    const response = await sendOwnerCommand(...args);
    if (args[1].command === 'scoped_capture_enqueue' && response.ok && loseReceipt) {
      loseReceipt = false;
      throw new Error('daemon_unavailable');
    }
    return response;
  } });
  t.after(() => fixture.close());
  const connection = {};
  let channel = await connect(fixture, [], connection);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`capture_${++n}`, `memex.${method}`, params);
  const request = { request_key: `remote_mobile_${'a'.repeat(32)}`, url: 'https://capture.example.invalid/article' };
  const before = await sendOwnerCommand(fixture.socket, { command: 'list', cursor: null });
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'public' });
  assert.equal((await rpc('capture.enqueue', request)).error.code, 'permission_denied');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'edit'] });
  assert.equal((await rpc('capture.enqueue', request)).error.code, 'permission_denied');
  assert.deepEqual((await sendOwnerCommand(fixture.socket, { command: 'list', cursor: null })).jobs, before.jobs);
  fixture.authorization.set(fixture.grant);
  assert.equal((await rpc('capture.enqueue', { ...request, principal: 'forged' })).error.code, 'invalid_request');
  assert.equal((await rpc('capture.enqueue', request)).error.code, 'unknown');
  const accepted = (await rpc('capture.enqueue', request)).result;
  assert(accepted.job_id);
  assert.equal((await rpc('capture.enqueue', request)).result.job_id, accepted.job_id);
  assert.equal((await rpc('capture.enqueue', { ...request, url: 'https://capture.example.invalid/other' })).error.code, 'request_identity_conflict');
  const stored = await untilResult(() => rpc('capture.status', { job_id: accepted.job_id }), 'succeeded');
  const entry = (await rpc('library.item.get', { item_id: stored.item_id })).result;
  assert.equal(entry.sensitivity, 'internal');
  assert.equal(entry.title, 'Synthetic capture article');
  assert.equal(entry.source_url, request.url);
  assert.match(entry.markdown, /synthetic article explains/);
  const status = await untilResult(() => rpc('library.translation.status', { item_id: stored.item_id }), 'succeeded');
  assert.deepEqual(Object.keys(status).sort(), ['progress', 'reason', 'revision_id', 'state', 'translated_title']);
  assert.equal(status.revision_id, entry.revision_id);
  const range = { item_id: stored.item_id, expected_revision_id: entry.revision_id, start_byte: 0 };
  let page = (await rpc('library.translation.read', range)).result;
  assert.equal(page.title, '合成收录文章');
  assert.equal(page.coordinate_space, 'analysis_translation_utf8_v1');
  assert(page.document_analysis_id && page.title_analysis_id);
  const hash = page.content_sha256;
  assert(page.next_byte > 0);
  let text = page.content_markdown;
  while (page.next_byte !== null) {
    page = (await rpc('library.translation.read', { ...range, start_byte: page.next_byte })).result;
    text += page.content_markdown;
  }
  assert.equal(text, `${'这是合成译文，专家依据证据审慎判断。'.repeat(400)}末尾界🙂`);
  assert.equal(hash, `sha256:${createHash('sha256').update(text).digest('hex')}`);
  assert.equal((await rpc('library.translation.read', { ...range, start_byte: 1 })).error.code, 'invalid_context_range');
  // Reconnect the same authenticated pairing: stable ownership/replay, not a new job.
  const disconnected = once(channel.socket, 'close');
  channel.close();
  await disconnected;
  await fixture.restartBridge();
  channel = await connect(fixture, [], connection);
  assert.equal((await rpc('capture.enqueue', request)).result.job_id, accepted.job_id);
  const previousPid = fixture.pid;
  await fixture.restartDaemon();
  assert.notEqual(fixture.pid, previousPid);
  assert.equal((await rpc('capture.enqueue', request)).result.job_id, accepted.job_id);
  assert.equal((await sendOwnerCommand(fixture.socket, { command: 'list', cursor: null })).jobs.length, 1);
  assert.equal((await rpc('library.query', { ...window, limit: 200 })).result.total, 4);
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'public' });
  assert.equal((await rpc('capture.status', { job_id: accepted.job_id })).error.code, 'permission_denied');
  assert.equal((await rpc('library.translation.status', { item_id: stored.item_id })).error.code, 'library_resource_not_found');
  assert.equal((await rpc('library.translation.read', range)).error.code, 'library_resource_not_found');
  fixture.authorization.set(fixture.grant);
  assert((await rpc('library.metadata.update', { item_id: stored.item_id, expected_revision_id: entry.revision_id,
    title: 'Revised captured source', manual_tags: [] })).result);
  assert.equal((await rpc('library.translation.read', range)).error.code, 'stale_item_revision');
  const current = (await rpc('library.translation.status', { item_id: stored.item_id })).result;
  assert.notEqual(current.revision_id, entry.revision_id);
  assert.equal(current.state, 'not_requested');
  channel.close();
  await fixture.rePair();
  channel = await connect(fixture);
  assert.equal((await rpc('capture.status', { job_id: accepted.job_id })).error.code, 'remote_resource_not_found');
  assert.equal((await rpc('capture.enqueue', request)).error.code, 'remote_resource_not_found');
  assert.equal((await sendOwnerCommand(fixture.socket, { command: 'list', cursor: null })).jobs.length, 1);
});

for (const change of ['revoke', 'expire', 'ceiling']) {
  test(`capture and translation ${change} suppress in-flight result frames`, async (t) => {
    let now = Date.now();
    let pendingCommand;
    let entered;
    let release;
    let gate;
    const fixture = await startMobileFixture({ capture: true, now: () => now, sendCommand: async (...args) => {
      const result = await sendOwnerCommand(...args);
      if (args[1].command === pendingCommand) { entered(); await gate; }
      return result;
    } });
    t.after(() => fixture.close());
    t.after(() => release?.());
    const frames = [];
    const channel = await connect(fixture, frames);
    t.after(() => channel.close());
    let n = 0;
    const rpc = (method, params) => channel.request(`inflight_${++n}`, `memex.${method}`, params);
    const accepted = (await rpc('capture.enqueue', { request_key: `remote_mobile_${'b'.repeat(32)}`,
      url: 'https://capture.example.invalid/article' })).result;
    const stored = await untilResult(() => rpc('capture.status', { job_id: accepted.job_id }), 'succeeded');
    const status = await untilResult(() => rpc('library.translation.status', { item_id: stored.item_id }), 'succeeded');
    pendingCommand = change === 'revoke' ? 'scoped_capture_status'
      : change === 'expire' ? 'scoped_translation_status' : 'scoped_translation_read';
    gate = new Promise((resolve) => { release = resolve; });
    const dispatched = new Promise((resolve) => { entered = resolve; });
    const result = change === 'revoke' ? rpc('capture.status', { job_id: accepted.job_id })
      : change === 'expire' ? rpc('library.translation.status', { item_id: stored.item_id })
        : rpc('library.translation.read', { item_id: stored.item_id, expected_revision_id: status.revision_id, start_byte: 0 });
    const rejected = assert.rejects(result);
    await dispatched;
    const frameCount = frames.length;
    if (change === 'revoke') fixture.authorization.revoke(fixture.grant.sessionId);
    if (change === 'expire') now = fixture.grant.expiresAt;
    if (change === 'ceiling') fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'public' });
    release();
    await rejected;
    assert.equal(frames.length, frameCount);
  });
}

test('translation failure, absent job and invalid Analysis never replace the original', async (t) => {
  const fixture = await startMobileFixture({ capture: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`failure_${++n}`, `memex.${method}`, params);
  for (const [path, expected] of [['fail-translation', 'failed'], ['chinese', 'not_requested'], ['article', 'succeeded']]) {
    const job = (await rpc('capture.enqueue', { request_key: `remote_mobile_${String(n).padStart(32, 'c')}`,
      url: `https://capture.example.invalid/${path}` })).result;
    const stored = await untilResult(() => rpc('capture.status', { job_id: job.job_id }), 'succeeded');
    const status = await untilResult(() => rpc('library.translation.status', { item_id: stored.item_id }), expected);
    const params = { item_id: stored.item_id, expected_revision_id: status.revision_id, start_byte: 0 };
    if (expected === 'succeeded') {
      const fs = require('node:fs');
      const page = (await rpc('library.translation.read', params)).result;
      // Corrupt only the disposable synthetic Analysis, not its queue success bit.
      const file = `${fixture.root}/Library/analysis/records/${page.document_analysis_id}.json`;
      fs.writeFileSync(file, '{}');
      assert.equal((await rpc('library.translation.status', { item_id: stored.item_id })).result.state, 'unavailable');
    }
    assert.equal((await rpc('library.translation.read', params)).error.code, 'translation_unavailable');
    assert((await rpc('library.item.get', { item_id: stored.item_id })).result.markdown.length > 0);
  }
});

test('minutes status, minutes request and translation request map to scoped owner commands', async (t) => {
  const seen = [];
  let loseReply = null;
  const fixture = await startMobileFixture({ sendCommand: async (socket, command, options) => {
    seen.push(command);
    const response = await sendOwnerCommand(socket, command, options);
    if (command.command === loseReply) throw new Error('daemon_unavailable');
    return response;
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`ai_${++n}`, `memex.${method}`, params);
  const visible = fixture.items[0].item_id;
  const hidden = fixture.items[2].item_id;
  const ceiling = fixture.grant.maxSensitivity;

  // Read: `read` scope only, the session ceiling, never authorizes anything.
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  assert.deepEqual((await rpc('library.minutes.status', { item_id: visible })).result,
    { state: 'not_requested', reason: null, text: null });
  assert.deepEqual(seen.at(-1), { command: 'scoped_minutes_status', item_id: visible, max_sensitivity: ceiling });
  assert.equal((await rpc('library.minutes.status', { item_id: hidden })).error.code, 'library_resource_not_found');
  for (const params of [{ item_id: visible, max_sensitivity: 'restricted' }, { item_id: visible, extra: 1 }, {}]) {
    assert.equal((await rpc('library.minutes.status', params)).error.code, 'invalid_request');
  }
  const dispatched = seen.length;
  // Requests need `capture`; `read` or `read`+`edit` never reach the daemon.
  for (const scopes of [['read'], ['read', 'edit']]) {
    fixture.authorization.set({ ...fixture.grant, scopes });
    for (const method of ['library.minutes.request', 'library.translation.request']) {
      assert.equal((await rpc(method, { item_id: visible })).error.code, 'permission_denied');
    }
  }
  assert.equal(seen.length, dispatched);
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'capture'] });
  for (const method of ['library.minutes.request', 'library.translation.request']) {
    assert.equal((await rpc(method, { item_id: visible, max_sensitivity: 'restricted' })).error.code, 'invalid_request');
    assert.equal((await rpc(method, { item_id: hidden })).error.code, 'library_resource_not_found');
  }
  assert.equal(seen.length, dispatched + 2, 'malformed requests are rejected before dispatch');
  // The disposable fixture has no model routes: the daemon's own answers pass through.
  assert.equal((await rpc('library.minutes.request', { item_id: visible })).error.code, 'no_minutes_provider');
  assert.deepEqual(seen.at(-1), { command: 'minutes_request', item_id: visible, max_sensitivity: ceiling });
  const requested = (await rpc('library.translation.request', { item_id: visible })).result;
  assert.deepEqual(seen.at(-1), { command: 'translation_request', item_id: visible, max_sensitivity: ceiling });
  assert.equal(requested.state, 'unavailable');
  assert.equal(requested.reason, 'no_translation_provider');
  assert.equal(requested.revision_id, fixture.items[0].revision_id);
  assert.deepEqual(requested.ai_minutes, { state: 'unavailable', reason: 'no_summary_provider', text: null });
  assert.deepEqual(Object.keys(requested).sort(),
    ['ai_minutes', 'progress', 'reason', 'revision_id', 'state', 'translated_title']);
  // A lost reply after dispatching a request is unknown (the phone reads status, never resends);
  // a lost read reply stays a plain read failure.
  for (const command of ['minutes_request', 'translation_request']) {
    loseReply = command;
    const method = command === 'minutes_request' ? 'library.minutes.request' : 'library.translation.request';
    assert.equal((await rpc(method, { item_id: visible })).error.code, 'unknown');
  }
  loseReply = 'scoped_minutes_status';
  assert.equal((await rpc('library.minutes.status', { item_id: visible })).error.code, 'daemon_unavailable');
  loseReply = null;
  // Nothing was authorized by any of the above.
  assert.equal((await rpc('library.minutes.status', { item_id: visible })).result.state, 'not_requested');
  // A larger session ceiling (never the client) makes the hidden item readable.
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'private' });
  assert.equal((await rpc('library.minutes.status', { item_id: hidden })).result.state, 'not_requested');
  assert.deepEqual(seen.at(-1), { command: 'scoped_minutes_status', item_id: hidden, max_sensitivity: 'private' });
});

test('insights status/request and conversation read map to scoped owner commands', async (t) => {
  const seen = [];
  let loseReply = null;
  const fixture = await startMobileFixture({ seedAi: true, sendCommand: async (socket, command, options) => {
    seen.push(command);
    const response = await sendOwnerCommand(socket, command, options);
    if (command.command === loseReply) throw new Error('daemon_unavailable');
    return response;
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  // Paced under the relay's 30 frames/s limit (this test sends many small requests).
  const rpc = async (method, params) => {
    await new Promise((resolve) => setTimeout(resolve, 50));
    return channel.request(`ins_${++n}`, `memex.${method}`, params);
  };
  const essay = fixture.items[4].item_id;
  const plain = fixture.items[0].item_id;
  const hidden = fixture.items[2].item_id;
  const ceiling = fixture.grant.maxSensitivity;
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const info = (await rpc('session.info', {})).result;
  assert.equal(info.insights, 'available');
  assert.equal(info.conversation, 'available');

  // insights.status: `read` only, session ceiling, real-shaped seeded items.
  const insights = (await rpc('library.insights.status', { item_id: essay })).result;
  assert.deepEqual(seen.at(-1), { command: 'scoped_insights_status', item_id: essay, max_sensitivity: ceiling });
  assert.deepEqual(Object.keys(insights).sort(), ['fact_check', 'highlights', 'quotes']);
  assert.equal(insights.highlights.state, 'succeeded');
  assert.equal(insights.highlights.items.length, 4);
  assert.deepEqual(Object.keys(insights.quotes.items[0]).sort(), ['block_index', 'quote', 'translation_zh', 'why']);
  assert.equal(insights.fact_check.basis, 'model_knowledge');
  assert.deepEqual(insights.fact_check.items.map((item) => item.verdict), ['unverifiable', 'opinion']);
  const none = (await rpc('library.insights.status', { item_id: plain })).result;
  assert.deepEqual([none.highlights.state, none.quotes.state, none.fact_check.state],
    ['not_requested', 'not_requested', 'not_requested']);
  assert.equal((await rpc('library.insights.status', { item_id: hidden })).error.code, 'library_resource_not_found');
  const dispatched = seen.length;
  for (const params of [{ item_id: essay, max_sensitivity: 'restricted' }, { item_id: essay, kinds: ['quotes'] }, {}]) {
    assert.equal((await rpc('library.insights.status', params)).error.code, 'invalid_request');
  }

  // insights.request: `capture` only; kinds validated exactly before dispatch.
  for (const scopes of [['read'], ['read', 'edit']]) {
    fixture.authorization.set({ ...fixture.grant, scopes });
    assert.equal((await rpc('library.insights.request', { item_id: plain, kinds: ['quotes'] })).error.code,
      'permission_denied');
  }
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'capture'] });
  for (const kinds of [[], ['summary'], ['quotes', 'quotes'], 'quotes', [1],
    ['highlights', 'quotes', 'fact_check', 'quotes']]) {
    assert.equal((await rpc('library.insights.request', { item_id: plain, kinds })).error.code, 'invalid_request');
  }
  assert.equal((await rpc('library.insights.request', { item_id: plain })).error.code, 'invalid_request');
  assert.equal((await rpc('library.insights.request',
    { item_id: plain, kinds: ['quotes'], max_sensitivity: 'restricted' })).error.code, 'invalid_request');
  for (const params of [{ item_id: plain }, { item_id: plain, cursor: '1', extra: 1 }, { item_id: plain, cursor: '--1' },
    { item_id: plain, cursor: 3 }, { item_id: plain, cursor: '1234567890123456789' }]) {
    assert.equal((await rpc('library.conversation.read', params)).error.code, 'invalid_request');
  }
  // conversation_import is owner-only: no gateway method maps to it, under any scope.
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'capture', 'edit'] });
  const restore = { item_id: plain, origin: 'restored_from_user_screenshot', turns: [{ role: 'user', content: 'x' }] };
  for (const method of ['library.conversation.import', 'library.conversation_import', 'conversation_import']) {
    assert.equal((await rpc(method, restore)).error.code, 'invalid_request');
  }
  assert.equal((await rpc('library.conversation.read', { ...restore, cursor: null })).error.code, 'invalid_request');
  assert.equal(seen.length, dispatched, 'malformed or unauthorized calls never reach the daemon');
  // The fixture has no model routes: the daemon's own answer passes through, nothing enqueued.
  assert.equal((await rpc('library.insights.request', { item_id: plain, kinds: ['quotes', 'fact_check'] })).error.code,
    'no_insight_provider');
  assert.deepEqual(seen.at(-1),
    { command: 'insight_request', item_id: plain, max_sensitivity: ceiling, kinds: ['quotes', 'fact_check'] });
  assert.equal((await rpc('library.insights.request', { item_id: hidden, kinds: ['quotes'] })).error.code,
    'library_resource_not_found');
  // A request reply carries states only (items are read with insights.status).
  loseReply = null;
  const done = (await rpc('library.insights.request', { item_id: essay, kinds: ['quotes'] }));
  assert.equal(done.error.code, 'no_insight_provider', 'route check precedes the succeeded shortcut');
  // A lost reply after dispatching a request is unknown; a lost read stays a read failure.
  loseReply = 'insight_request';
  assert.equal((await rpc('library.insights.request', { item_id: plain, kinds: ['quotes'] })).error.code, 'unknown');
  loseReply = 'scoped_insights_status';
  assert.equal((await rpc('library.insights.status', { item_id: plain })).error.code, 'daemon_unavailable');
  loseReply = 'conversation_read';
  assert.equal((await rpc('library.conversation.read', { item_id: plain, cursor: null })).error.code, 'daemon_unavailable');
  loseReply = null;
  assert.equal((await rpc('library.insights.status', { item_id: plain })).result.quotes.state, 'not_requested');

  // conversation.read: `read` only, oldest first, never mixed across items.
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const history = (await rpc('library.conversation.read', { item_id: essay, cursor: null })).result;
  assert.deepEqual(seen.at(-1), { command: 'conversation_read', item_id: essay, max_sensitivity: ceiling, cursor: null });
  assert.equal(history.item_id, essay);
  // Restored (screenshot) turns first, marked with their origin, then the stored chat turns.
  assert.deepEqual(history.turns.map((turn) => turn.role), ['user', 'assistant', 'user', 'assistant', 'user', 'assistant']);
  assert.deepEqual(history.turns.map((turn) => turn.origin),
    ['restored_from_user_screenshot', 'restored_from_user_screenshot', null, null, null, null]);
  assert(history.turns[0].seq < 0 && history.turns[0].request_id.startsWith('restored:'));
  assert.equal(history.turns[2].content, '这篇文章的核心观点是什么？');
  assert.equal(history.turns[5].model, 'Synthetic Sol');
  assert.equal(history.next_cursor, null);
  assert.match(history.version, /^v1:6:\d+$/);
  const after = (await rpc('library.conversation.read', { item_id: essay, cursor: String(history.turns[1].seq) })).result;
  assert.deepEqual(after.turns, history.turns.slice(2));
  const empty = (await rpc('library.conversation.read', { item_id: plain, cursor: null })).result;
  assert.deepEqual([empty.turns, empty.version, empty.next_cursor], [[], 'v1:0:0', null]);
  assert.equal((await rpc('library.conversation.read', { item_id: hidden, cursor: null })).error.code,
    'library_resource_not_found');
  // A failed chat turn (no chat model in the fixture) appends nothing.
  assert.equal((await rpc('library.item.chat', { item_id: essay, request_id: 'gateway-history-fail-0001',
    messages: [{ role: 'user', content: '再问一句' }] })).error.code, 'no_chat_provider');
  assert.deepEqual((await rpc('library.conversation.read', { item_id: essay, cursor: null })).result, history);
  // A larger session ceiling (never the client) makes the hidden item readable.
  fixture.authorization.set({ ...fixture.grant, maxSensitivity: 'private' });
  assert.deepEqual((await rpc('library.conversation.read', { item_id: hidden, cursor: null })).result.turns, []);
  assert.deepEqual(seen.at(-1), { command: 'conversation_read', item_id: hidden, max_sensitivity: 'private', cursor: null });
});

test('groups.tree, group-filtered query and session versions map to scoped owner reads', async (t) => {
  const seen = [];
  let failVersion = false;
  let oldVersion = false;
  const fixture = await startMobileFixture({ sendCommand: async (socket, command, options) => {
    seen.push(command);
    if (failVersion && command.command === 'agent_version') throw new Error('daemon_unavailable');
    if (oldVersion && command.command === 'agent_version') return { ok: true, agent_version: { version: '0.1.0', binary_sha256: null } };
    return sendOwnerCommand(socket, command, options);
  } });
  t.after(() => fixture.close());
  const owner = (command) => sendOwnerCommand(fixture.socket, { max_sensitivity: 'restricted', ...command });
  // Owner-side synthetic tree: a private root with one folder holding a public and a hidden item.
  const root = await owner({ command: 'group_edit', request_id: 'tree-r1', expected_version: null,
    ops: [{ op: 'create_root', name: 'Reading' }] });
  const rootId = root.group_edit.group_id;
  const folder = await owner({ command: 'group_edit', request_id: 'tree-r2',
    expected_version: root.group_tree.nodes.find((n) => n.id === rootId).version,
    ops: [{ op: 'create_folder', parent_id: rootId, name: 'Papers' }] });
  const folderId = folder.group_edit.group_id;
  const added = await owner({ command: 'group_edit', request_id: 'tree-r3',
    expected_version: folder.group_tree.nodes.find((n) => n.id === folderId).version,
    ops: [{ op: 'add_members', group_id: folderId, item_ids: [fixture.items[0].item_id, fixture.items[2].item_id] }] });
  assert.equal(added.ok, true);

  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = async (method, params) => {
    await new Promise((resolve) => setTimeout(resolve, 50));
    return channel.request(`grp_${++n}`, `memex.${method}`, params);
  };
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const info = (await rpc('session.info', {})).result;
  assert.equal(info.groups, 'available');
  assert.equal(info.groups_v1.query_unfiled, true);
  assert.equal(info.groups_v1.node_delete, false);
  assert.deepEqual(info.query_sorts, ['title', 'source', 'captured_at', 'added_at', 'item_type']);
  assert.equal(info.query_origin, 'available');
  assert.deepEqual(Object.keys(info.versions).sort(), ['agent', 'gateway', 'web_bundle']);
  assert.match(info.versions.gateway, /^[0-9a-f]{12}$/);
  assert.equal(info.versions.web_bundle, null);
  assert.deepEqual(Object.keys(info.versions.agent).sort(), ['binary_sha256', 'version']);
  assert.match(info.versions.agent.version, /^\d+\.\d+\.\d+/);
  assert.deepEqual(seen.at(-1), { command: 'agent_version' });
  oldVersion = true;
  assert.equal((await rpc('session.info', {})).result.query_origin, 'unavailable');
  oldVersion = false;
  failVersion = true;
  assert.equal((await rpc('session.info', {})).result.versions.agent, null, 'an unreachable daemon is null, not a guess');
  failVersion = false;

  // The session ceiling (public) hides the private root, and so its folder.
  const window = { offset: 0, limit: 50, expected_view: null };
  const hiddenTree = (await rpc('library.groups.tree', window)).result;
  assert.deepEqual(seen.at(-1), { command: 'group_tree_read', max_sensitivity: 'public', ...window });
  assert.deepEqual([hiddenTree.total, hiddenTree.nodes], [0, []]);
  const inboxParams = { ...window, query: { ...query, unfiled: true } };
  const publicInbox = (await rpc('library.query', inboxParams)).result;
  assert(publicInbox.items.some((item) => item.item_id === fixture.items[0].item_id));
  assert(publicInbox.items.every((item) => item.item_id !== fixture.items[2].item_id));
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'], maxSensitivity: 'private' });
  const privateInbox = (await rpc('library.query', inboxParams)).result;
  assert.equal(privateInbox.total, 2);
  assert(privateInbox.items.every((item) => ![fixture.items[0].item_id, fixture.items[2].item_id].includes(item.item_id)));
  assert.deepEqual(privateInbox.groups ?? [], []);
  const tree = (await rpc('library.groups.tree', window)).result;
  assert.deepEqual(seen.at(-1), { command: 'group_tree_read', max_sensitivity: 'private', ...window });
  assert.deepEqual(Object.keys(tree).sort(), ['next_offset', 'nodes', 'offset', 'total', 'view']);
  assert.deepEqual(tree.nodes.map((node) => [node.name, node.kind, node.parent_id === null]),
    [['Reading', 'user', true], ['Papers', 'collection', false]]);
  assert.deepEqual(Object.keys(tree.nodes[0]).sort(),
    ['id', 'item_count', 'kind', 'name', 'parent_id', 'read_only', 'root_group_id', 'subtree_item_count', 'version'],
    'versions support CAS; sensitivity and owner metadata stay on the Mac');
  assert.equal(tree.nodes[1].subtree_item_count, tree.nodes[0].subtree_item_count);
  assert.equal((await rpc('library.groups.tree', { ...window, offset: 0, expected_view: 'v-not-current' })).error.code,
    'stale_group_view');

  // Selecting a node filters the query by group_id + include_descendants.
  const queried = (await rpc('library.query', { query: { ...query, sort: 'added_at', group_id: rootId, include_descendants: true },
    offset: 0, limit: 10, expected_view: null })).result;
  assert.equal(seen.at(-1).query.group_id, rootId);
  assert.equal(seen.at(-1).query.sort, 'added_at');
  assert(queried.items.every((item) => /^2026-/.test(item.added_at)));
  assert.equal(seen.at(-1).max_sensitivity, 'private');
  assert(queried.items.some((item) => item.item_id === fixture.items[0].item_id));
  assert.equal(queried.total, tree.nodes[0].subtree_item_count);
  const direct = (await rpc('library.query', { query: { ...query, group_id: rootId, include_descendants: false },
    offset: 0, limit: 10, expected_view: null })).result;
  assert.equal(direct.total, 0, 'the root itself holds nothing directly');

  for (const sort of info.query_sorts) {
    const result = await rpc('library.query', { query: { ...query, sort },
      offset: 0, limit: 5, expected_view: null });
    assert.equal(result.result.total, 4, `advertised sort ${sort} is usable`);
  }
  for (const [origin, count] of [['capture', 3], ['import', 1], [null, 4]]) {
    const filtered = (await rpc('library.query', { query: { ...query, origin },
      offset: 0, limit: 5, expected_view: null })).result;
    assert.equal(filtered.total, count);
    assert.equal(filtered.items.length, count);
    assert.equal(filtered.sources.reduce((n, f) => n + f.count, 0), count);
    if (origin) assert(filtered.items.every(i => i.origin === origin));
  }
  const dispatched = seen.length;
  for (const params of [{ ...window, limit: 0 }, { ...window, limit: 501 }, { ...window, offset: -1 },
    { ...window, offset: 0.5 }, { ...window, expected_view: 3 }, { offset: 0, limit: 5 },
    { ...window, max_sensitivity: 'restricted' }]) {
    assert.equal((await rpc('library.groups.tree', params)).error.code, 'invalid_request');
  }
  for (const q of [{ ...query, group_id: rootId }, { ...query, include_descendants: true },
    { ...query, group_id: 'bad id/..', include_descendants: true }, { ...query, group_id: rootId, include_descendants: 'yes' },
    { ...query, group_id: '', include_descendants: false }, { ...query, sort: 'published_at' }, { ...query, origin: 'web' }]) {
    assert.equal((await rpc('library.query', { query: q, offset: 0, limit: 5, expected_view: null })).error.code,
      'invalid_request');
  }
  assert.equal(seen.length, dispatched, 'malformed calls never reach the daemon');
});

test('library root ordering persists with Main pinned, edit scope and stable retry identity', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`order_${++n}`, `memex.${method}`, params);
  const create = async (name, sensitivity = 'public') => {
    const r = await sendOwnerCommand(fixture.socket, { command: 'group_edit', max_sensitivity: 'private',
      request_id: `create-${name}`, expected_version: null, ops: [{ op: 'create_root', name, sensitivity }] });
    assert(r.ok); return r.group_edit;
  };
  const z = await create('Zulu'); const a = await create('Alpha'); const m = await create('Main');
  const hidden = await create('Hidden', 'private');
  const names = async () => (await rpc('library.groups.tree', { offset: 0, limit: 50, expected_view: null })).result.nodes.map(n => n.name);
  assert.deepEqual(await names(), ['Main', 'Zulu', 'Alpha']);
  const move = { group_id: a.group_id, before_id: z.group_id, expected_version: a.version, request_id: 'move-alpha-0000001' };
  assert.equal((await rpc('session.info', {})).result.group_reorder, 'available');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  assert.equal((await rpc('session.info', {})).result.group_reorder, 'unavailable');
  assert.equal((await rpc('library.groups.reorder', move)).error.code, 'permission_denied');
  fixture.authorization.set(fixture.grant);
  const receipt = (await rpc('library.groups.reorder', move)).result;
  assert(receipt);
  assert.deepEqual((await rpc('library.groups.reorder', move)).result, receipt);
  assert.deepEqual(await names(), ['Main', 'Alpha', 'Zulu']);
  assert.equal((await rpc('library.groups.reorder', { ...move, request_id: 'move-alpha-0000002' })).error.code, 'stale_group_version');
  assert.equal((await rpc('library.groups.reorder', { ...move, request_id: 'move-main-00000001', group_id: m.group_id, expected_version: m.version })).error.code, 'invalid_group_edit');
  assert.equal((await rpc('library.groups.reorder', { ...move, before_id: hidden.group_id })).error.code, 'library_resource_not_found');
  await fixture.restartDaemon();
  assert.deepEqual(await names(), ['Main', 'Alpha', 'Zulu']);
});

test('library trash is edit-scoped, recoverable and replay-safe over encrypted RPC', async (t) => {
  const fixture = await startMobileFixture();
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`trash_${++n}`, `memex.${method}`, params);
  const r = await sendOwnerCommand(fixture.socket, { command: 'group_edit', max_sensitivity: 'public', request_id: 'trash-create', expected_version: null,
    ops: [{ op: 'create_root', name: 'Main', sensitivity: 'public' }] });
  const group_id = r.group_edit.group_id;
  const item = fixture.items[0];
  assert((await sendOwnerCommand(fixture.socket, { command: 'group_edit', max_sensitivity: 'public', request_id: 'trash-members', expected_version: r.group_edit.version,
    ops: [{ op: 'add_members', group_id, item_ids: [item.item_id] }] })).ok);
  const remove = { group_id, item_id: item.item_id, expected_revision_id: item.revision_id, request_id: 'trash-request-00001', restore_plan: null };
  assert.equal((await rpc('session.info', {})).result.group_trash, 'available');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  assert.equal((await rpc('library.trash.edit', remove)).error.code, 'permission_denied');
  assert.equal((await rpc('library.trash.list', { group_id, cursor: null })).result.items.length, 0);
  fixture.authorization.set(fixture.grant);
  assert.equal((await rpc('library.trash.edit', { ...remove, max_sensitivity: 'private' })).error.code, 'invalid_request');
  assert.equal((await rpc('library.trash.edit', remove)).result.applied, true);
  const listed = (await rpc('library.trash.list', { group_id, cursor: null })).result;
  assert.equal(listed.items.length, 1);
  assert.equal(listed.items[0].item_id, item.item_id);
  const filtered = { ...window, query: { ...query, group_id, include_descendants: true } };
  assert.equal((await rpc('library.query', filtered)).result.total, 0);
  await fixture.restartDaemon();
  assert.equal((await rpc('library.trash.edit', { ...remove, request_id: 'trash-restore-0001', restore_plan: listed.items[0].plan_id })).result.applied, true);
  assert.equal((await rpc('library.trash.edit', remove)).result.applied, true);
  assert.equal((await rpc('library.query', filtered)).result.total, 1);
  assert.equal((await rpc('library.trash.list', { group_id, cursor: null })).result.items.length, 0);
});

test('large local images share relay bandwidth, remain revision scoped and never expose file paths', async (t) => {
  const fs = require('node:fs');
  const path = require('node:path');
  const fixture = await startMobileFixture({ seedImages: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`rpc_image_${++n}`, `memex.${method}`, params);
  const item = fixture.items[3];
  const original = fs.readFileSync(path.join(__dirname, '../../crates/memex-agent/examples/fixtures/reading-image.png'));
  const hash = createHash('sha256').update(original).digest('hex');
  const target = `attachments/${hash}.png`;
  const base = { item_id: item.item_id, expected_revision_id: item.revision_id, target, offset: 0 };
  const read = async () => {
    const chunks = []; let offset = 0;
    do {
      const reply = await rpc('library.image.read', { ...base, offset });
      assert.equal(reply.error, undefined);
      const r = reply.result;
      assert.equal(r.revision_id, item.revision_id);
      assert.equal(r.offset, offset);
      chunks.push(Buffer.from(r.data_base64, 'base64'));
      offset = r.next_offset;
    } while (offset !== null);
    assert.deepEqual(Buffer.concat(chunks), original);
  };
  await Promise.all([read(), read(), rpc('library.query', window).then(r => assert.equal(r.result.total, 2))]);
  assert.equal((await rpc('session.info', {})).result.item_cover_v1, 'available');
  const batch = (await rpc('library.item_cover_batch', { item_ids: [item.item_id, fixture.items[2].item_id] })).result;
  assert.equal(batch[1], null);
  const cover = batch[0];
  assert.equal(cover.source_revision_id, item.revision_id);
  assert.equal(cover.raster.sha256, hash);
  assert(cover.thumbnail.width <= 320 && cover.thumbnail.height <= 320);
  await fixture.restartDaemon();
  for (const thumbnail of [false, true]) {
    const chunks = []; let offset = 0;
    do {
      const reply = await rpc('library.item_cover_chunk', { item_id: item.item_id,
        version: cover.version, thumbnail, offset });
      assert.equal(reply.error, undefined);
      assert.equal(reply.result.offset, offset);
      chunks.push(Buffer.from(reply.result.data_base64, 'base64'));
      offset = reply.result.next_offset;
    } while (offset !== null);
    const bytes = Buffer.concat(chunks);
    assert.equal(createHash('sha256').update(bytes).digest('hex'),
      (thumbnail ? cover.thumbnail : cover.raster).sha256);
    if (!thumbnail) assert.deepEqual(bytes, original);
  }
  assert.equal((await rpc('library.item_cover_chunk', { item_id: item.item_id,
    version: '0'.repeat(64), thumbnail: false, offset: 0 })).error.code, 'stale_cover_version');
  assert.equal((await rpc('session.info', {})).error, undefined);
  assert.equal((await rpc('library.image.read', { ...base, max_sensitivity: 'private' })).error.code, 'invalid_request');
  assert.equal((await rpc('library.image.read', { ...base, target: '../outside.png' })).error.code, 'invalid_request');
  assert.equal((await rpc('library.image.read', { ...base, expected_revision_id: 'revision_wrong' })).error.code, 'stale_item_revision');
  assert.equal((await rpc('library.image.read', { ...base, ...{ item_id: fixture.items[2].item_id, expected_revision_id: fixture.items[2].revision_id } })).error.code, 'image_resource_not_found');
  const object = path.join(fixture.root, 'Imports/synthetic/raw/objects', hash);
  const damaged = Buffer.from(original); damaged[50] ^= 1; fs.writeFileSync(object, damaged);
  assert.equal((await rpc('library.image.read', base)).error.code, 'image_read_failed');
});

test('reader TOC/chunks use the real daemon over encrypted RPC with strict read-only inputs', async (t) => {
  const seen = [];
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    seen.push(args[1]);
    return sendOwnerCommand(...args);
  } });
  t.after(() => fixture.close());
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`reader_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.reader, 'available');
  const item = fixture.items[0];
  const toc = (await rpc('library.reader.toc', item)).result;
  assert.deepEqual(seen.at(-1), { command: 'reader', max_sensitivity: 'public', request: { op: 'toc', ...item } });
  assert.equal(toc.default_side, 'source');
  assert.equal(toc.editions[0].chapters[0].key, 'section-0');
  const params = { ...item, side: 'source', chapter: 'section-0', offset: 16383, limit: 4 };
  const first = (await rpc('library.reader.chunk', params)).result;
  assert.deepEqual(seen.at(-1), { command: 'reader', max_sensitivity: 'public', request: { op: 'chunk', ...params } });
  assert.deepEqual([first.markdown, first.start, first.end, first.next_offset], ['界', 16383, 16386, 16386]);
  assert.equal(first.sha256, toc.canonical_sha256);
  assert.equal(first.evidence_id, null);
  const next = (await rpc('library.reader.chunk', { ...params, offset: first.next_offset, limit: 32768 })).result;
  assert.deepEqual([next.markdown, next.end, next.next_offset], ['🙂Z', 16391, null]);
  for (const [patch, code] of [
    [{ side: 'original' }, 'reader_side_not_found'],
    [{ chapter: 'absent' }, 'reader_chapter_not_found'],
    [{ offset: 16384 }, 'invalid_reader_offset'],
    [{ offset: 16392 }, 'invalid_reader_offset'],
    [{ revision_id: 'revision_wrong' }, 'stale_item_revision'],
    [{ item_id: 'item_absent' }, 'library_resource_not_found'],
    [{ ...fixture.items[2], revision_id: 'revision_wrong' }, 'library_resource_not_found'],
  ]) assert.equal((await rpc('library.reader.chunk', { ...params, ...patch })).error.code, code);
  const before = seen.length;
  for (const [patch, code] of [
    [{ offset: -1 }, 'invalid_reader_offset'], [{ offset: 0.5 }, 'invalid_reader_offset'],
    [{ offset: Number.MAX_SAFE_INTEGER + 1 }, 'invalid_reader_offset'],
    [{ limit: 3 }, 'invalid_reader_limit'], [{ limit: 32769 }, 'invalid_reader_limit'],
    [{ limit: 4.5 }, 'invalid_reader_limit'], [{ limit: '4' }, 'invalid_reader_limit'],
    [{ side: 'both' }, 'invalid_reader_request'], [{ chapter: '' }, 'invalid_reader_request'],
    [{ chapter: 'x'.repeat(257) }, 'invalid_reader_request'],
    [{ max_sensitivity: 'restricted' }, 'invalid_reader_request'],
    [{ op: 'save' }, 'invalid_reader_request'], [{ revision_id: '../revision' }, 'invalid_request'],
    [{ revision_id: null }, 'invalid_request'], [{ revision_id: undefined }, 'invalid_reader_request'],
  ]) assert.equal((await rpc('library.reader.chunk', { ...params, ...patch })).error.code, code);
  assert.equal((await rpc('library.reader.toc', { ...item, side: 'source' })).error.code, 'invalid_reader_request');
  assert.equal((await rpc('library.reader.toc', { item_id: item.item_id })).error.code, 'invalid_reader_request');
  assert.equal((await rpc('library.reader.save', { ...item, expected_version: 'a'.repeat(64), state: {} })).error.code, 'permission_denied');
  assert.equal(seen.length, before, 'invalid/read-write requests never reach the daemon');
  const alignment = { ...item, chapter: 'section-0', offset: 0, limit: 128, expected_version: 'any' };
  assert.equal((await rpc('library.reader.alignment', alignment)).error.code, 'reader_side_not_found');
  assert.deepEqual(seen.at(-1), { command: 'reader', max_sensitivity: 'public', request: { op: 'alignment', ...alignment } });
  const alignBefore = seen.length;
  for (const patch of [{ offset: -1 }, { offset: 1 }, { offset: 0.5 }, { limit: 0 }, { limit: 257 },
    { expected_version: '' }, { expected_version: null }, { chapter: '' }, { op: 'save' },
    { max_sensitivity: 'private' }, { expected_version: undefined }]) {
    assert.equal((await rpc('library.reader.alignment', { ...alignment, ...patch })).error.code, 'invalid_reader_alignment_request');
  }
  assert.equal(seen.length, alignBefore);
  assert(!frames.join('\n').includes('PRIVATE_CANARY_NOT_FOR_PUBLIC'));
  assert(!frames.join('\n').includes('界'));
});

test('reader paragraph pages use real persisted pairs and survive daemon restart', async (t) => {
  const fixture = await startMobileFixture({ seedParagraphs: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`paragraph_${++n}`, `memex.${method}`, params);
  const book = fixture.readerBook;
  const params = { item_id: book.item_id, revision_id: book.revision_id, chapter: 'chapter-1',
    offset: 0, limit: 2, expected_version: 'any' };
  const first = (await rpc('library.reader.alignment', params)).result;
  assert.equal(first.status, 'partial');
  assert.equal(first.total, 6);
  assert.equal(first.next_offset, 2);
  const pairs = [...first.pairs];
  for (const offset of [2, 4]) {
    const page = (await rpc('library.reader.alignment', { ...params, offset, expected_version: first.version })).result;
    pairs.push(...page.pairs);
  }
  assert.deepEqual(pairs.map(p => [p.kind, p.relation]), [
    ['heading', 'matched'], ['paragraph', 'matched'], ['paragraph', 'matched'],
    ['note', 'translation_only'], ['paragraph', 'unresolved'], ['paragraph', 'unresolved'],
  ]);
  const contents = {};
  for (const [side, key] of [['original', 'o'], ['translation', 't']]) {
    const chunk = (await rpc('library.reader.chunk', { item_id: book.item_id, revision_id: book.revision_id,
      side, chapter: 'chapter-1', offset: 0, limit: 32768 })).result;
    const body = Buffer.from(chunk.markdown);
    assert.equal(createHash('sha256').update(body).digest('hex'), first[`${side}_sha256`]);
    let end = 0;
    contents[side] = [];
    for (const pair of pairs) if (pair[key]) {
      assert.equal(pair[key][0], end);
      contents[side].push(body.subarray(...pair[key]).toString('utf8'));
      end = pair[key][1];
    }
    assert.equal(end, body.length);
  }
  assert.equal(contents.original[1], 'One morning.\n\n');
  assert.equal(contents.translation[1], '清晨。\n\n两个片段。\n\n');
  assert.equal(contents.original[2], 'Two birds.\n\nThree trees.\n\n');
  assert.equal(contents.translation[2], '鸟和树。\n\n');
  const appendix = (await rpc('library.reader.alignment', { ...params, chapter: 'appendix' })).result;
  assert.equal(appendix.version, 'missing');
  assert.equal(appendix.translation_chapter, null);
  assert.deepEqual(appendix.pairs, []);
  assert.equal((await rpc('library.reader.alignment', { ...params, expected_version: '0'.repeat(64) })).error.code, 'stale_chapter_alignment');
  assert.equal((await rpc('library.reader.alignment', { ...params, revision_id: 'revision_wrong' })).error.code, 'stale_item_revision');
  await fixture.restartDaemon();
  assert.deepEqual((await rpc('library.reader.alignment', params)).result, first);
});

test('reader discovery is capability-based and reader errors remain sanitized', async (t) => {
  let capability = 'live';
  let failure = null;
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    if (args[1].command === 'capabilities' && capability !== 'live') {
      if (capability === 'offline') throw new Error('daemon_unavailable');
      return capability;
    }
    if (args[1].command === 'reader' && failure) return { ok: false, reason_code: failure, detail: 'PRIVATE_ERROR_CANARY' };
    return sendOwnerCommand(...args);
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`reader_cap_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.reader, 'available');
  assert.equal((await rpc('session.info', {})).result.reader_alignment, 'available');
  assert.equal((await rpc('session.info', {})).result.reader_repair, 'available');
  for (const response of ['offline', { ok: false }, { ok: true },
    { ok: true, capabilities: { features: [] } },
    { ok: true, capabilities: { features: 'reader_v1' } }]) {
    capability = response;
    const info = (await rpc('session.info', {})).result;
    assert.equal(info.reader, 'unavailable');
    assert.equal(info.reader_alignment, 'unavailable');
    assert.equal(info.reader_repair, 'unavailable');
  }
  for (const code of ['invalid_reader_request', 'invalid_reader_offset', 'invalid_reader_limit',
    'invalid_reader_alignment_request', 'stale_chapter_alignment',
    'reader_side_not_found', 'reader_chapter_not_found', 'stale_book_manifest',
    'stale_item_revision', 'library_resource_not_found', 'PRIVATE_ERROR_CANARY']) {
    failure = code;
    const expected = code === 'PRIVATE_ERROR_CANARY' ? 'invalid_request' : code;
    const response = await rpc('library.reader.toc', fixture.items[0]);
    assert.deepEqual(response.error, { code: expected, message: expected });
    assert.equal(response.result, undefined);
  }
});

test('revoking a reader request in flight suppresses its encrypted response', async (t) => {
  const gate = gatedFixture('reader');
  const fixture = await gate.start();
  t.after(() => fixture.close());
  t.after(() => gate.release());
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  const rejected = assert.rejects(channel.request('reader_pending', 'memex.library.reader.toc', fixture.items[0]));
  await gate.reached;
  const sent = frames.length;
  fixture.authorization.revoke(fixture.grant.sessionId);
  gate.release();
  await rejected;
  assert.equal(frames.length, sent);
});

test('optional bilingual fixture adds a book and serves hash-bound UTF8 chapter chunks', async (t) => {
  const fixture = await startMobileFixture({ seedReader: true });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`reader_book_${++n}`, `memex.${method}`, params);
  const { item_id, revision_id } = fixture.readerBook;
  const item = { item_id, revision_id };
  assert.equal((await rpc('session.info', {})).result.reader, 'available');
  const listing = (await rpc('library.query', { ...window, limit: 200 })).result;
  assert.equal(listing.total, 3);
  assert(listing.items.some((i) => i.item_id === item_id));
  assert(listing.items.some((i) => i.item_id === fixture.items[0].item_id));
  assert(listing.items.some((i) => i.item_id === fixture.items[3].item_id));
  const toc = (await rpc('library.reader.toc', item)).result;
  assert.deepEqual(toc, fixture.readerBook);
  assert.deepEqual(toc.editions.map((e) => [e.side, e.language, e.chapters.length]),
    [['original', 'en', 3], ['translation', 'zh', 2]]);
  assert.deepEqual(toc.editions[0].chapters.map((c) => c.counterpart), ['chapter-1', 'chapter-2', null]);
  const entry = (await rpc('library.item.get', { item_id })).result;
  assert.equal(entry.content_sha256, `sha256:${toc.canonical_sha256}`);
  for (const edition of toc.editions) {
    let body = '';
    for (const chapter of edition.chapters) {
      let offset = 0;
      do {
        const chunk = (await rpc('library.reader.chunk', { ...item, side: edition.side,
          chapter: chapter.key, offset, limit: 17 })).result;
        assert.equal(chunk.start, chapter.start + offset);
        assert(chunk.end <= chapter.end);
        assert.equal(Buffer.byteLength(chunk.markdown), chunk.end - chunk.start);
        assert(chunk.end - chunk.start <= 17);
        assert(!chunk.markdown.includes('\ufffd'));
        assert.equal(chunk.sha256, edition.sha256);
        assert.equal(chunk.revision_id, revision_id);
        assert.match(chunk.evidence_id, /^evidence_synthetic_reader_/);
        body += chunk.markdown;
        offset = chunk.next_offset;
      } while (offset !== null);
    }
    assert.equal(Buffer.byteLength(body), edition.bytes);
    assert.equal(createHash('sha256').update(body).digest('hex'), edition.sha256);
    if (edition.side === 'translation') assert.equal(body, entry.markdown);
  }
  assert.equal((await rpc('library.reader.chunk', { ...item, side: 'translation', chapter: 'chapter-1', offset: 3, limit: 4 })).error.code, 'invalid_reader_offset');
  assert.equal((await rpc('library.reader.chunk', { ...item, side: 'translation', chapter: 'appendix', offset: 0, limit: 4 })).error.code, 'reader_chapter_not_found');
  assert.equal((await rpc('library.reader.toc', { ...item, revision_id: 'revision_wrong' })).error.code, 'stale_item_revision');
  await fixture.restartDaemon();
  assert.deepEqual((await rpc('library.reader.toc', item)).result, toc);
  const changed = await sendOwnerCommand(fixture.socket, { command: 'markdown_edit', max_sensitivity: 'public',
    edit: { item_id, expected_revision_id: revision_id, request_id: 'reader-fixture-edit-0001', markdown: 'Changed synthetic canonical body.' } });
  assert.equal(changed.ok, true);
  const current = (await rpc('library.item.get', { item_id })).result;
  assert.equal((await rpc('library.reader.toc', item)).error.code, 'stale_item_revision');
  assert.equal((await rpc('library.reader.toc', { ...item, revision_id: current.revision_id })).error.code, 'stale_book_manifest');
});

test('B2 reader state sync: capability follows daemon reader support', async () => {
  const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'b2-cap', generation: 1,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: 10000 });
  let features = ['reader_v1'];
  const route = createLibraryRouter({ socketPath: '/not-used', authorization,
    sendCommand: async () => ({ ok: true, capabilities: { features } }) });
  const info = async () => (await route(JSON.stringify({ id: 'i', method: 'memex.session.info', params: {} }), grant)).result;
  assert.equal((await info()).reader_state_sync_v1, 'available');
  features = [];
  assert.equal((await info()).reader_state_sync_v1, 'unavailable');
});

test('B2 reader state sync over encrypted gateway keeps CAS, bookmarks and strict admission', async (t) => {
  const seen = [];
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    seen.push(args[1]);
    return sendOwnerCommand(...args);
  } });
  t.after(() => fixture.close());
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`b2_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.reader_state_sync_v1, 'available');
  const item = fixture.items[0];
  const toc = (await rpc('library.reader.toc', item)).result;
  const initial = (await rpc('library.reader.state', { item_id: item.item_id })).result;
  assert.deepEqual(seen.at(-1), { command: 'reader', max_sensitivity: 'public', request: { op: 'state', item_id: item.item_id } });
  assert.deepEqual(initial.state, { visit_id: '', position: null, bookmarks: [] });
  const anchor = (offset) => ({ revision_id: item.revision_id, side: 'source', sha256: toc.editions[0].sha256, chapter: 'section-0', offset });
  const mark = { id: 'bm-1', label: '书签 1', anchor: anchor(16383) };
  const first = { visit_id: 'phone-open-1', position: anchor(16383), bookmarks: [mark] };
  // Read-only sessions cannot save, and nothing reaches the daemon.
  let before = seen.length;
  assert.equal((await rpc('library.reader.save', { item_id: item.item_id, expected_version: initial.version, state: first })).error.code, 'permission_denied');
  assert.equal(seen.length, before);
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'edit'] });
  const saved = (await rpc('library.reader.save', { item_id: item.item_id, expected_version: initial.version, state: first })).result;
  assert.deepEqual(saved.state, first);
  assert.deepEqual(seen.at(-1).request.op, 'save');
  assert.equal(seen.at(-1).max_sensitivity, 'public');
  // Identical replay (lost reply retry) keeps the same version.
  assert.equal((await rpc('library.reader.save', { item_id: item.item_id, expected_version: initial.version, state: first })).result.version, saved.version);
  // A second device holding the old version conflicts instead of overwriting.
  const other = { visit_id: 'mac-open-1', position: anchor(0), bookmarks: [] };
  assert.equal((await rpc('library.reader.save', { item_id: item.item_id, expected_version: initial.version, state: other })).error.code, 'stale_reader_state');
  // Position updates keep the bookmark and the visit.
  const moved = { ...first, position: anchor(16386) };
  const second = (await rpc('library.reader.save', { item_id: item.item_id, expected_version: saved.version, state: moved })).result;
  assert.deepEqual(second.state.bookmarks, [mark]);
  assert.deepEqual((await rpc('library.reader.resume', {})).result, [item.item_id, moved]);
  // Store validates anchors: a non-UTF8 boundary and a wrong revision are definite rejections.
  for (const [position, code] of [[anchor(16384), 'invalid_reader_anchor'],
    [{ ...anchor(0), revision_id: 'revision_wrong' }, 'stale_item_revision']]) {
    assert.equal((await rpc('library.reader.save', { item_id: item.item_id, expected_version: second.version,
      state: { ...moved, position } })).error.code, code);
  }
  // Strict shapes never reach the daemon: injected ceiling/principal, extra or missing keys.
  before = seen.length;
  for (const params of [
    { item_id: item.item_id, expected_version: second.version, state: moved, max_sensitivity: 'restricted' },
    { item_id: item.item_id, expected_version: second.version, state: { ...moved, principal: 'x' } },
    { item_id: item.item_id, expected_version: 'missing', state: moved },
    { item_id: item.item_id, state: moved },
    { item_id: item.item_id, expected_version: second.version, state: { ...moved, bookmarks: null } },
  ]) assert.equal((await rpc('library.reader.save', params)).error.code, 'invalid_reader_state');
  assert.equal((await rpc('library.reader.state', { item_id: item.item_id, op: 'save' })).error.code, 'invalid_reader_state');
  assert.equal((await rpc('library.reader.resume', { item_id: item.item_id })).error.code, 'invalid_reader_state');
  assert.equal(seen.length, before);
  // A private item is invisible at the session ceiling, for read and write alike.
  const hidden = fixture.items[2];
  assert.equal((await rpc('library.reader.state', { item_id: hidden.item_id })).error.code, 'library_resource_not_found');
  assert.equal((await rpc('library.reader.save', { item_id: hidden.item_id, expected_version: initial.version,
    state: { visit_id: 'phone-open-2', position: null, bookmarks: [] } })).error.code, 'library_resource_not_found');
});

test('B2 lost save reply is unknown and the attempted state is readable afterwards', async (t) => {
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    const response = await sendOwnerCommand(...args);
    if (args[1].command === 'reader' && args[1].request.op === 'save') throw new Error('daemon_unavailable');
    return response;
  } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  const item = fixture.items[0];
  const initial = (await channel.request('s', 'memex.library.reader.state', { item_id: item.item_id })).result;
  const state = { visit_id: 'phone-open-lost', position: null, bookmarks: [] };
  const lost = await channel.request('w', 'memex.library.reader.save', { item_id: item.item_id, expected_version: initial.version, state });
  assert.equal(lost.error.code, 'unknown');
  assert.deepEqual((await channel.request('r', 'memex.library.reader.state', { item_id: item.item_id })).result.state, state);
});

test('B2 save admitted before revocation may commit but its reply is never sent', async (t) => {
  let release; let entered;
  const gate = new Promise((resolve) => { release = resolve; });
  const reached = new Promise((resolve) => { entered = resolve; });
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    const response = await sendOwnerCommand(...args);
    if (args[1].command === 'reader' && args[1].request.op === 'save') { entered(); await gate; }
    return response;
  } });
  t.after(() => fixture.close());
  t.after(() => release());
  const frames = [];
  const channel = await connect(fixture, frames);
  t.after(() => channel.close());
  const item = fixture.items[0];
  const initial = (await channel.request('s', 'memex.library.reader.state', { item_id: item.item_id })).result;
  const state = { visit_id: 'phone-open-revoked', position: null, bookmarks: [] };
  const pending = assert.rejects(channel.request('w', 'memex.library.reader.save', { item_id: item.item_id, expected_version: initial.version, state }));
  await reached;
  const sent = frames.length;
  fixture.authorization.revoke(fixture.grant.sessionId);
  release();
  await pending;
  assert.equal(frames.length, sent);
  const readback = await sendOwnerCommand(fixture.socket, { command: 'reader', max_sensitivity: 'public',
    request: { op: 'state', item_id: item.item_id } });
  assert.deepEqual(readback.reader.state, state);
});

test('reader_annotations_v1 over encrypted gateway: CAS, exact replay, scope and strict shapes', async (t) => {
  const seen = [];
  const fixture = await startMobileFixture({ sendCommand: async (...args) => { seen.push(args[1]); return sendOwnerCommand(...args); } });
  t.after(() => fixture.close());
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`ann_${++n}`, `memex.${method}`, params);
  assert.equal((await rpc('session.info', {})).result.reader_annotations_v1, 'available');
  const item = fixture.items[0];
  const toc = (await rpc('library.reader.toc', item)).result;
  const empty = (await rpc('library.reader.annotations.list', { item_id: item.item_id, offset: 0, limit: 10, expected_version: null })).result;
  assert.deepEqual([empty.total, empty.annotations, empty.next_offset], [0, [], null]);
  const annotation = { id: 'mark-1', style: 'red', anchor: { revision_id: item.revision_id, side: 'source',
    sha256: toc.editions[0].sha256, segments: [{ chapter: 'section-0', start: 16383, end: 16386 }] } };
  const put = { item_id: item.item_id, expected_version: empty.version, change: { op: 'put', annotation } };
  let before = seen.length;
  assert.equal((await rpc('library.reader.annotations.edit', put)).error.code, 'permission_denied');
  assert.equal(seen.length, before);
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'edit'] });
  const created = (await rpc('library.reader.annotations.edit', put)).result;
  assert.deepEqual([created.id, created.outcome], ['mark-1', 'created']);
  assert.equal(seen.at(-1).max_sensitivity, 'public');
  assert.deepEqual(await rpc('library.reader.annotations.edit', put).then((r) => r.result), created, 'exact retry is idempotent');
  const listed = (await rpc('library.reader.annotations.list', { item_id: item.item_id, offset: 0, limit: 10, expected_version: null })).result;
  assert.deepEqual([listed.total, listed.annotations[0].status, listed.annotations[0].style], [1, 'current', 'red']);
  // Non-UTF8 boundary and stale CAS are definite rejections.
  assert.equal((await rpc('library.reader.annotations.edit', { ...put, expected_version: created.version,
    change: { op: 'put', annotation: { ...annotation, id: 'mark-2', anchor: { ...annotation.anchor, segments: [{ chapter: 'section-0', start: 16384, end: 16386 }] } } } })).error.code, 'invalid_reader_anchor');
  assert.equal((await rpc('library.reader.annotations.edit', { item_id: item.item_id, expected_version: empty.version,
    change: { op: 'delete', id: 'mark-1' } })).error.code, 'stale_reader_annotations');
  before = seen.length;
  for (const params of [
    { ...put, max_sensitivity: 'restricted' },
    { ...put, change: { op: 'put', annotation: { ...annotation, style: 'green' } } },
    { ...put, change: { op: 'move', id: 'mark-1' } },
    { ...put, change: { op: 'put', annotation: { ...annotation, principal: 'x' } } },
    { ...put, expected_version: 'missing' },
  ]) assert.equal((await rpc('library.reader.annotations.edit', params)).error.code, 'invalid_reader_annotation');
  assert.equal((await rpc('library.reader.annotations.list', { item_id: item.item_id, offset: 1, limit: 10, expected_version: null })).error.code, 'invalid_reader_annotation');
  assert.equal(seen.length, before);
  const deleted = (await rpc('library.reader.annotations.edit', { item_id: item.item_id, expected_version: created.version,
    change: { op: 'delete', id: 'mark-1' } })).result;
  assert.equal(deleted.outcome, 'deleted');
  assert.notEqual(deleted.version, empty.version, 'versions never repeat');
  // Reading position/bookmarks are untouched by mark edits.
  assert.deepEqual((await rpc('library.reader.state', { item_id: item.item_id })).result.state, { visit_id: '', position: null, bookmarks: [] });
  assert.equal((await rpc('library.reader.annotations.list', { item_id: fixture.items[2].item_id, offset: 0, limit: 10, expected_version: null })).error.code, 'library_resource_not_found');
});

test('reader_annotations_ai_translation_v1 over encrypted gateway: separate ledger, CAS, stale and view-bound pages', async (t) => {
  const seen = [];
  const fixture = await startMobileFixture({ translationProjection: 'legacy',
    sendCommand: async (...args) => { seen.push(args[1]); return sendOwnerCommand(...args); } });
  t.after(() => fixture.close());
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`aiann_${++n}`, `memex.${method}`, params);
  const info = (await rpc('session.info', {})).result;
  assert.equal(info.reader_annotations_ai_translation_v1, 'available');
  assert.equal(info.reader_annotations_v1, 'available');
  const { item_id, revision_id } = fixture.readerBook;
  const selection = { item_id, revision_id, chapter: 'appendix' };
  const meta = (await rpc('library.reader.translate_metadata', selection)).result.translation;
  const body = (await rpc('library.reader.translate_chunk', { ...selection, expected_version: meta.version, offset: 0, limit: 32768 })).result;
  const bytes = Buffer.from(body.markdown);
  const at = bytes.indexOf(Buffer.from('合成'));
  const seg = (start, end, m = meta) => ({ chapter: 'appendix', translation_version: m.version, sha256: m.sha256, start, end });
  const space = { annotation_space: 'ai_translation' };
  const list = (extra = {}) => rpc('library.reader.annotations.list', { item_id, offset: 0, limit: 100, expected_version: null, ...extra });
  const legacy0 = (await list()).result;
  const ai0 = (await list(space)).result;
  assert.notEqual(ai0.version, legacy0.version);
  assert.equal(ai0.annotation_space, 'ai_translation');
  const put = (id, segments, style = 'red') => ({ op: 'put', annotation: { id, style,
    anchor: { revision_id, side: 'ai_translation', segments } } });
  const edit = (expected_version, change, extra = space) => rpc('library.reader.annotations.edit', { item_id, expected_version, change, ...extra });
  // Strict admission: AI anchor without space, edition anchor in AI space, unknown space/keys never reach the daemon.
  const before = seen.length;
  for (const reply of [
    await edit(legacy0.version, put('m', [seg(at, at + 6)]), {}),
    await edit(ai0.version, { op: 'put', annotation: { id: 'm', style: 'red', anchor: { revision_id, side: 'original', sha256: meta.sha256, segments: [{ chapter: 'appendix', start: 0, end: 3 }] } } }),
    await edit(ai0.version, put('m', [{ ...seg(at, at + 6), extra: 1 }])),
    await edit(ai0.version, put('m', [seg(at, at + 6)]), { annotation_space: 'summary' }),
    await list({ annotation_space: 'ai_translation', projection: { translation_version: meta.version } }),
    await list({ expected_view_version: ai0.view_version }),
    await list({ ...space, offset: 1, expected_version: ai0.version }),
  ]) assert.equal(reply.error.code, 'invalid_reader_annotation');
  assert.equal(seen.length, before);
  // Same id in both ledgers; UTF-8 boundary rejection; CAS and exact replay.
  assert.equal((await edit(ai0.version, put('m', [seg(at + 1, at + 6)]))).error.code, 'invalid_reader_anchor');
  const created = (await edit(ai0.version, put('m', [seg(at, at + 6)]))).result;
  assert.deepEqual([created.outcome, created.annotation_space], ['created', 'ai_translation']);
  assert.deepEqual((await edit(ai0.version, put('m', [seg(at, at + 6)]))).result, created);
  assert.equal((await edit(ai0.version, put('k', [seg(at, at + 3)]))).error.code, 'stale_reader_annotations');
  const toc = (await rpc('library.reader.toc', { item_id, revision_id })).result;
  const original = toc.editions.find((e) => e.side === 'original');
  const legacyPut = { op: 'put', annotation: { id: 'm', style: 'red', anchor: { revision_id, side: 'original', sha256: original.sha256,
    segments: [{ chapter: 'chapter-1', start: 0, end: 3 }] } } };
  const legacyMark = (await edit(legacy0.version, legacyPut, {})).result;
  assert.deepEqual(Object.keys(legacyMark).sort(), ['id', 'item_id', 'outcome', 'version']);
  // The old list shape never carries AI marks or fields.
  const legacy = (await list()).result;
  assert.deepEqual(Object.keys(legacy).sort(), ['annotations', 'item_id', 'next_offset', 'total', 'version']);
  assert.deepEqual(legacy.annotations.map((m) => [m.id, m.anchor.side, Object.hasOwn(m, 'projections')]), [['m', 'original', false]]);
  // A delete without space only affects the edition ledger.
  assert.equal((await edit(legacy.version, { op: 'delete', id: 'm' }, {})).result.outcome, 'deleted');
  const second = (await edit(created.version, put('n', [seg(at + 6, at + 12)]))).result;
  const filter = { chapter: 'appendix', translation_version: meta.version, sha256: meta.sha256 };
  const page1 = (await list({ ...space, limit: 1, projection: filter })).result;
  assert.deepEqual([page1.total, page1.next_offset, page1.annotations[0].projections[0].status], [2, 1, 'current']);
  const page2 = (await list({ ...space, offset: 1, limit: 1, projection: filter, expected_version: page1.version, expected_view_version: page1.view_version })).result;
  assert.deepEqual(page2.annotations.map((m) => m.id), ['n']);
  // The translation changes (same body, new projection version): pages cannot mix, marks go stale.
  const file = path.join(fixture.root, 'Library/catalog/chapter-translations', item_id, revision_id,
    `${createHash('sha256').update('appendix').digest('hex')}.json`);
  const projection = JSON.parse(fs.readFileSync(file));
  projection.provenance.prompt_version = 'synthetic-v2';
  fs.writeFileSync(file, JSON.stringify(projection));
  assert.equal((await list({ ...space, offset: 1, limit: 1, projection: filter, expected_version: page1.version,
    expected_view_version: page1.view_version })).error.code, 'stale_reader_annotations');
  const stale = (await list(space)).result;
  assert.equal(stale.version, second.version);
  assert.deepEqual(stale.annotations.map((m) => [m.id, m.status]), [['m', 'stale'], ['n', 'stale']]);
  // Stale marks are delete-only; writing the old projection is the determinate stale code.
  assert.equal((await edit(second.version, put('m', [seg(at, at + 6)], 'blue'))).error.code, 'stale_reader_translation');
  assert.equal((await edit(second.version, put('o', [seg(at, at + 3)]))).error.code, 'stale_reader_translation');
  const removed = (await edit(second.version, { op: 'delete', id: 'm' })).result;
  assert.equal(removed.outcome, 'deleted');
  assert.deepEqual((await list(space)).result.annotations.map((m) => m.id), ['n']);
  // Edit scope is required.
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  assert.equal((await edit(removed.version, { op: 'delete', id: 'n' })).error.code, 'permission_denied');
});

test('reader_passes_v1 over encrypted gateway: CAS, timing replay, fences, scope, strict shapes and lost replies', async (t) => {
  const seen = [];
  let dropReply = false;
  const fixture = await startMobileFixture({ sendCommand: async (...args) => {
    seen.push(args[1]);
    const reply = await sendOwnerCommand(...args);
    if (dropReply) { dropReply = false; throw new Error('socket_closed'); }
    return reply;
  } });
  t.after(() => fixture.close());
  fixture.authorization.set({ ...fixture.grant, scopes: ['read'] });
  const channel = await connect(fixture);
  t.after(() => channel.close());
  let n = 0;
  const rpc = (method, params) => channel.request(`pass_${++n}`, `memex.library.reader.passes.${method}`, params);
  assert.equal((await channel.request('info', 'memex.session.info', {})).result.reader_passes_v1, 'available');
  const item = fixture.items[0];
  const list = () => rpc('list', { item_id: item.item_id, offset: 0, limit: 10, expected_version: null }).then((r) => r.result);
  const empty = await list();
  assert.deepEqual([empty.total, empty.passes, empty.next_offset, empty.pass_limit], [0, [], null, 4096]);
  const create = { item_id: item.item_id, expected_version: empty.version, change: { op: 'create_tracked',
    pass_id: 'pass_gateway', revision_id: item.revision_id, method: 'full', start_date: null, timezone: 'Asia/Shanghai' } };
  let before = seen.length;
  for (const [method, params] of [['edit', create], ['timing_start', { item_id: item.item_id, pass_id: 'pass_gateway' }],
    ['timing_report', { item_id: item.item_id, pass_id: 'pass_gateway', session_id: 'tsess_x',
      segments: [{ segment_id: 's', mode: 'source', start_offset_ms: 0, end_offset_ms: 1 }] }]]) {
    assert.equal((await rpc(method, params)).error.code, 'permission_denied', method);
  }
  assert.equal(seen.length, before, 'read-only grants dispatch no pass writes');
  fixture.authorization.set({ ...fixture.grant, scopes: ['read', 'edit'] });
  const created = (await rpc('edit', create)).result;
  assert.deepEqual([created.pass_id, created.ordinal, created.outcome], ['pass_gateway', 1, 'created']);
  assert.equal(seen.at(-1).max_sensitivity, 'public');
  assert.match(seen.at(-1).principal, /^[a-f0-9]{64}$/);
  assert.deepEqual((await rpc('edit', create)).result, created, 'exact retry is idempotent');
  const session = (await rpc('timing_start', { item_id: item.item_id, pass_id: 'pass_gateway' })).result;
  assert.deepEqual([session.window_ms, session.max_segment_ms], [300000, 300000]);
  await new Promise((resolve) => setTimeout(resolve, 60));
  const report = { item_id: item.item_id, pass_id: 'pass_gateway', session_id: session.session_id,
    segments: [{ segment_id: 'seg-1', mode: 'bilingual', start_offset_ms: 0, end_offset_ms: 40 }] };
  // A lost reply is unknown, never a rejection; the exact replay then reconciles as duplicate.
  dropReply = true;
  assert.equal((await rpc('timing_report', report)).error.code, 'unknown');
  const replay = (await rpc('timing_report', report)).result;
  assert.deepEqual([replay.outcomes, replay.totals.unique_ms, replay.totals.bilingual_ms],
    [[{ segment_id: 'seg-1', outcome: 'duplicate' }], 40, 40]);
  assert.equal((await rpc('timing_report', { ...report, segments: [{ ...report.segments[0], mode: 'source' }] })).error.code,
    'reader_pass_segment_conflict');
  assert.equal((await rpc('timing_report', { ...report, segments: [{ segment_id: 'seg-f', mode: 'source',
    start_offset_ms: 0, end_offset_ms: 3600000 }] })).error.code, 'invalid_reader_pass_timing');
  const stats = (await rpc('stats', { item_id: item.item_id, pass_id: 'pass_gateway', offset: 0, limit: 10,
    expected_timing_version: null })).result;
  assert.deepEqual([stats.timezone, stats.days.reduce((a, d) => a + d.ms, 0)], ['Asia/Shanghai', 40]);
  // The heartbeat left the metadata version alone; completion fences new time.
  const listed = await list();
  assert.equal(listed.version, created.version);
  assert.deepEqual([listed.passes[0].display_rank, listed.passes[0].timing.unique_ms, listed.passes[0].rates.basis], [1, 40, 'measured']);
  const complete = { item_id: item.item_id, expected_version: created.version, change: { op: 'complete', pass_id: 'pass_gateway',
    end_date: { value: '2026-11-08', precision: 'day' }, expected_timing_version: stats.timing_version } };
  assert.equal((await rpc('edit', complete)).result.outcome, 'completed');
  assert.equal((await rpc('timing_report', { ...report, segments: [{ segment_id: 'seg-2', mode: 'source',
    start_offset_ms: 40, end_offset_ms: 50 }] })).error.code, 'reader_pass_closed');
  assert.equal((await rpc('edit', { ...create, change: { ...create.change, pass_id: 'pass_other' } })).error.code, 'stale_reader_passes');
  before = seen.length;
  const retro = { op: 'create_retrospective', pass_id: 'pass_r', method: null, start_date: null, end_date: null,
    estimated_duration_minutes: 90, duration_display_unit: 'hours', timezone: 'Asia/Shanghai', reference_revision_id: null };
  for (const [method, params] of [
    ['edit', { ...create, max_sensitivity: 'restricted' }],
    ['edit', { ...create, change: { ...create.change, principal: 'x' } }],
    ['edit', { ...create, change: { ...create.change, op: 'convert' } }],
    ['edit', { ...create, change: { ...create.change, method: 'skim' } }],
    ['edit', { ...create, change: { ...retro, estimated_duration_minutes: 1.5 } }],
    ['edit', { ...create, change: { ...retro, estimated_duration_minutes: 525601 } }],
    ['edit', { ...create, change: { ...retro, start_date: { value: '2024', precision: 'unparsed' } } }],
    ['edit', { ...create, expected_version: 'missing' }],
    ['list', { item_id: item.item_id, offset: 1, limit: 10, expected_version: null }],
    ['stats', { item_id: item.item_id, pass_id: 'pass_gateway', offset: 0, limit: 401, expected_timing_version: null }],
    ['timing_start', { item_id: item.item_id, pass_id: 'pass_gateway', session_id: 'tsess_x' }],
    ['timing_report', { ...report, segments: [] }],
    ['timing_report', { ...report, segments: Array.from({ length: 65 }, (_, i) => ({ segment_id: `s${i}`, mode: 'source', start_offset_ms: i, end_offset_ms: i + 1 })) }],
    ['timing_report', { ...report, segments: [{ ...report.segments[0], mode: 'audio' }] }],
    ['timing_report', { ...report, segments: [{ ...report.segments[0], device_time: 1 }] }],
  ]) assert.ok(['invalid_reader_pass', 'invalid_reader_pass_timing'].includes((await rpc(method, params)).error.code), JSON.stringify(params));
  assert.equal(seen.length, before, 'malformed requests are rejected before dispatch');
  assert.equal((await rpc('list', { item_id: fixture.items[2].item_id, offset: 0, limit: 10, expected_version: null })).error.code,
    'library_resource_not_found');
  // Reading position and annotations are untouched.
  assert.deepEqual((await channel.request('st', 'memex.library.reader.state', { item_id: item.item_id })).result.state,
    { visit_id: '', position: null, bookmarks: [] });
});
