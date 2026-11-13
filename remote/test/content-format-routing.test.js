'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { SessionGrants, createLibraryRouter } = require('../mac/library-router');

const principal = 'a'.repeat(64);
const selection = { kind: 'item', item_id: 'item_test', revision_id: 'revision_test', target: { kind: 'body' } };
const list = { batch_id: 'batch_test', target_id: 'target_test', expected_version: 'b'.repeat(64), view: 'list', offset: 0, limit: 64 };
const text = { batch_id: 'batch_test', target_id: 'target_test', expected_version: 'b'.repeat(64), view: 'text', change_id: 'change_test', part: 'before', start_byte: 0, limit_bytes: 8192 };
const cases = [
  ['preview', { selection }], ['preview', { preview_id: 'preview_test' }],
  ['preview', { selection: { kind: 'group', group_id: 'group_test', include_descendants: false } }],
  ['preview', { selection: { ...selection, target: { kind: 'chapter', side: 'original', chapter: 'chapter-1' } } }],
  ['targets', { preview_id: 'preview_test', offset: 0, limit: 100 }],
  ['request', { quote_id: 'quote_test', request_id: 'request_test', accept_cost: true, accept_model_processing: true }],
  ['status', { batch_id: 'batch_test' }], ['results', { batch_id: 'batch_test', offset: 0, limit: 100 }],
  ['changes', list], ['changes', text],
  ['restore', { batch_id: 'batch_test', target_id: 'target_test', expected_revision_id: 'revision_test', request_id: 'restore_test' }],
  ['resume', { batch_id: 'batch_test' }],
];
const writes = ['request', 'restore', 'resume'];
function setup(sendCommand, scopes = ['read', 'edit']) {
  const authorization = new SessionGrants({ now: () => 0 });
  const grant = authorization.set({ sessionId: 'format-test', generation: 1, scopes, maxSensitivity: 'private', expiresAt: 10000 });
  const route = createLibraryRouter({ socketPath: '/unused', authorization, sendCommand });
  const rpc = (op, params, identity = principal) => route(JSON.stringify({ id: 'test', method: `memex.library.reader.format_${op}`, params }), grant, identity);
  return { authorization, grant, rpc, route };
}

test('format routing forwards all frozen shapes with only server authority and no extra envelope', async () => {
  const commands = [];
  const reply = { batch: { batch_id: 'batch_test' } };
  const { rpc } = setup(async (_, command) => { commands.push(command); return { ok: true, reader: reply }; });
  for (const [op, params] of cases) {
    assert.deepEqual((await rpc(op, params)).result, reply);
    assert.deepEqual(commands.at(-1), { command: 'reader', max_sensitivity: 'private', principal, request: { op: `format_${op}`, ...params } });
    const count = commands.length;
    for (const field of ['principal', 'max_sensitivity', 'unexpected']) {
      assert.equal((await rpc(op, { ...params, [field]: 'injected' })).error.code, 'invalid_content_format_request');
    }
    for (const field of Object.keys(params)) {
      const missing = { ...params }; delete missing[field];
      assert((await rpc(op, missing)).error);
    }
    assert.equal(commands.length, count);
  }
});

test('format exclusive shapes, paging, nested fields and consent reject before owner dispatch', async () => {
  let calls = 0;
  const { rpc } = setup(async () => { calls++; return { ok: true, reader: {} }; });
  for (const [op, params] of [
    ['preview', { selection, preview_id: 'preview_test' }],
    ['preview', { selection: { ...selection, target: { kind: 'body', side: 'original' } } }],
    ['preview', { selection: { kind: 'group', group_id: 'group_test' } }],
    ['preview', { selection: { kind: 'group', group_id: 'group_test', include_descendants: 1 } }],
    ['targets', { preview_id: 'preview_test', offset: -1, limit: 1 }],
    ['results', { batch_id: 'batch_test', offset: 0, limit: 101 }],
    ['changes', { ...list, change_id: 'change_test' }], ['changes', { ...text, offset: 0 }],
    ['changes', { ...list, limit: 65 }], ['changes', { ...text, limit_bytes: 8193 }],
    ['changes', { ...text, part: 'both' }], ['changes', { ...list, expected_version: 'any' }],
    ['request', { ...cases.find(([op]) => op === 'request')[1], accept_cost: false }],
    ['request', { ...cases.find(([op]) => op === 'request')[1], accept_model_processing: false }],
  ]) assert.equal((await rpc(op, params)).error.code, 'invalid_content_format_request');
  assert.equal((await rpc('status', { batch_id: 'batch_test' }, 'local_owner')).error.code, 'permission_denied');
  assert.equal(calls, 0);
});

test('format write scope, sanitized failures and uncertain writes retain existing semantics', async () => {
  let calls = 0;
  const readonly = setup(async () => { calls++; return { ok: true, reader: {} }; }, ['read']);
  for (const [op, params] of cases) {
    const response = await readonly.rpc(op, params);
    if (writes.includes(op)) assert.equal(response.error.code, 'permission_denied');
    else assert(response.result);
  }
  assert.equal(calls, cases.length - 3);
  for (const code of ['content_format_quote_consumed', 'content_format_quote_stale', 'content_format_restore_conflict',
    'content_format_batch_not_found', 'permission_denied', 'internal_error', 'untrusted provider prose']) {
    const { rpc } = setup(async () => ({ ok: false, reason_code: code }));
    for (const [op, params] of cases.filter(([op]) => writes.includes(op))) {
      assert.equal((await rpc(op, params)).error.code,
        ['internal_error', 'untrusted provider prose'].includes(code) ? 'unknown' : code);
    }
  }
  const { rpc } = setup(async () => ({ ok: false, reason_code: 'untrusted provider prose' }));
  assert.deepEqual((await rpc('status', { batch_id: 'batch_test' })).error,
    { code: 'invalid_request', message: 'invalid_request' });
});

test('format revocation before dispatch denies and revocation during dispatch suppresses receipts', async () => {
  for (const [op, params] of cases) {
    let calls = 0;
    const before = setup(async () => { calls++; return { ok: true, reader: { secret: 'not-returned' } }; });
    before.authorization.revoke(before.grant.sessionId);
    assert.equal((await before.rpc(op, params)).error.code, 'session_revoked');
    assert.equal(calls, 0);
    const during = setup(async () => {
      during.authorization.revoke(during.grant.sessionId);
      return { ok: true, reader: { secret: 'not-returned' } };
    });
    const response = await during.rpc(op, params);
    assert.equal(response.error.code, writes.includes(op) ? 'unknown' : 'session_revoked');
    assert.equal(response.result, undefined);
  }
});
