'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { createRpcRouter } = require('../mac/rpc-router');

test('RPC allowlist maps only the five bounded methods to owner UDS commands', async () => {
  const commands = [];
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async (_, command) => {
      commands.push(command);
      if (command.command === 'library_list') {
        return {
          ok: true,
          library_items: [{
            item_id: 'item_fixture', revision_id: 'revision_1', title: 'Fixture',
            item_type: 'webpage', captured_at: '2026-01-01T00:00:00Z',
          }],
          next_cursor: null,
        };
      }
      if (command.command === 'library_item_read') {
        return {
          ok: true,
          library_item: {
            item_id: command.item_id,
            revision_id: 'revision_1',
            summary_state: 'succeeded',
            summary: '经过校验的摘要',
            translation_state: 'succeeded',
            title: '合成翻译',
            markdown: '# 校验后的翻译',
          },
        };
      }
      if (command.command === 'remote_item_read') {
        return { ok: true, item: { item_id: command.item_id, revision_id: 'revision_1', title: 'Public fixture', markdown: '# Result' } };
      }
      return { ok: true, job: { job_id: 'job_00000000000000000000000000000000', state: 'queued', item_id: null } };
    },
  });
  const requestKey = 'remote_phone_stable_fixture';
  const enqueue = await router(Buffer.from(JSON.stringify({
    id: 'rpc_1',
    method: 'memex.capture.enqueue',
    params: { request_key: requestKey, url: 'https://public.example/article' },
  })));
  assert.equal(enqueue.result.state, 'queued');
  await router(Buffer.from(JSON.stringify({
    id: 'rpc_2', method: 'memex.capture.status',
    params: { job_id: 'job_00000000000000000000000000000000' },
  })));
  const read = await router(Buffer.from(JSON.stringify({
    id: 'rpc_3', method: 'memex.item.read', params: {
      job_id: 'job_00000000000000000000000000000000', item_id: 'item_fixture',
    },
  })));
  assert.equal(read.result.markdown, '# Result');
  const list = await router(Buffer.from(JSON.stringify({
    id: 'rpc_4', method: 'memex.library.list', params: { cursor: null },
  })));
  assert.equal(list.result.items[0].item_id, 'item_fixture');
  assert.equal(list.result.next_cursor, null);
  const libraryRead = await router(Buffer.from(JSON.stringify({
    id: 'rpc_5', method: 'memex.library.item.read',
    params: { item_id: 'item_fixture' },
  })));
  assert.equal(libraryRead.result.summary_state, 'succeeded');
  assert.equal(libraryRead.result.summary, '经过校验的摘要');
  assert.equal(libraryRead.result.translation_state, 'succeeded');
  assert.equal(libraryRead.result.markdown, '# 校验后的翻译');
  assert.deepEqual(commands, [
    {
      command: 'enqueue',
      producer: 'remote_iphone',
      producer_job_id: requestKey,
      url: 'https://public.example/article',
    },
    { command: 'remote_status', job_id: 'job_00000000000000000000000000000000' },
    {
      command: 'remote_item_read',
      job_id: 'job_00000000000000000000000000000000',
      item_id: 'item_fixture',
    },
    { command: 'library_list', cursor: null },
    { command: 'library_item_read', item_id: 'item_fixture' },
  ]);
});

test('library item RPC exposes only complete validated generated projections', async () => {
  let libraryItem;
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => ({ ok: true, library_item: libraryItem }),
  });
  const read = () => router(Buffer.from(JSON.stringify({
    id: 'rpc_translation', method: 'memex.library.item.read',
    params: { item_id: 'item_fixture' },
  })));

  for (const translation_state of [
    'not_requested', 'queued', 'running', 'failed', 'unavailable',
  ]) {
    libraryItem = {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      summary_state: 'not_requested', summary: null,
      translation_state, title: null, markdown: null,
    };
    assert.equal((await read()).result.translation_state, translation_state);
  }
  for (const malformed of [
    {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      summary_state: 'not_requested', summary: null,
      translation_state: 'succeeded', title: null, markdown: null,
    },
    {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      summary_state: 'not_requested', summary: null,
      translation_state: 'queued', title: 'source must not escape', markdown: null,
    },
    {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      summary_state: 'not_requested', summary: null,
      translation_state: 'unknown', title: null, markdown: null,
    },
  ]) {
    libraryItem = malformed;
    const response = await read();
    assert.equal(response.error.code, 'internal_error');
    assert.equal(JSON.stringify(response).includes('source must not escape'), false);
  }

  for (const summary_state of [
    'not_requested', 'queued', 'running', 'failed', 'unavailable',
  ]) {
    libraryItem = {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      summary_state, summary: null,
      translation_state: 'not_requested', title: null, markdown: null,
    };
    assert.equal((await read()).result.summary_state, summary_state);
  }
  libraryItem = {
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'succeeded', summary: '经过校验的摘要',
    translation_state: 'not_requested', title: null, markdown: null,
  };
  assert.equal((await read()).result.summary, '经过校验的摘要');
  for (const malformed of [
    { summary_state: 'succeeded', summary: null },
    { summary_state: 'queued', summary: 'source must not escape' },
    { summary_state: 'unknown', summary: null },
  ]) {
    libraryItem = {
      item_id: 'item_fixture', revision_id: 'revision_fixture',
      ...malformed,
      translation_state: 'not_requested', title: null, markdown: null,
    };
    const response = await read();
    assert.equal(response.error.code, 'internal_error');
    assert.equal(JSON.stringify(response).includes('source must not escape'), false);
  }
});

test('RPC rejects extra fields, unknown methods, and returns body-free errors', async () => {
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => { throw new Error('must_not_run'); },
  });
  const privateValue = 'https://public.example/private-query?token=synthetic';
  const response = await router(Buffer.from(JSON.stringify({
    id: 'rpc_bad', method: 'memex.capture.delete', params: {}, url: privateValue,
  })));
  const encoded = JSON.stringify(response);
  assert.equal(response.error.code, 'invalid_request');
  assert.equal(encoded.includes(privateValue), false);
  assert.deepEqual(Object.keys(response.error).sort(), ['code', 'message']);
});

test('RPC replaces an escaped over-limit item result with a bounded public error', async () => {
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => ({
      ok: true,
      item: {
        item_id: 'item_fixture',
        revision_id: 'rev_fixture',
        title: 'Synthetic',
        markdown: '\\'.repeat(256 * 1024),
      },
    }),
  });
  const response = await router(Buffer.from(JSON.stringify({
    id: 'rpc_large', method: 'memex.item.read', params: {
      job_id: 'job_00000000000000000000000000000000', item_id: 'item_fixture',
    },
  })));
  assert.deepEqual(response, {
    id: 'rpc_large',
    error: { code: 'result_too_large', message: 'result_too_large' },
  });

  const libraryRouter = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => ({
      ok: true,
      library_item: {
        item_id: 'item_fixture',
        revision_id: 'rev_fixture',
        summary_state: 'not_requested',
        summary: null,
        translation_state: 'succeeded',
        title: '合成翻译',
        markdown: '\\'.repeat(256 * 1024),
      },
    }),
  });
  const libraryResponse = await libraryRouter(Buffer.from(JSON.stringify({
    id: 'rpc_large_library', method: 'memex.library.item.read',
    params: { item_id: 'item_fixture' },
  })));
  assert.deepEqual(libraryResponse, {
    id: 'rpc_large_library',
    error: { code: 'result_too_large', message: 'result_too_large' },
  });

  const aggregateRouter = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => ({
      ok: true,
      library_item: {
        item_id: 'item_fixture',
        revision_id: 'rev_fixture',
        summary_state: 'not_requested',
        summary: null,
        translation_state: 'succeeded',
        title: '合成标题'.repeat(1024),
        markdown: '译'.repeat(78 * 1024),
      },
    }),
  });
  const aggregateResponse = await aggregateRouter(Buffer.from(JSON.stringify({
    id: 'rpc_aggregate_library', method: 'memex.library.item.read',
    params: { item_id: 'item_fixture' },
  })));
  assert.deepEqual(aggregateResponse, {
    id: 'rpc_aggregate_library',
    error: { code: 'result_too_large', message: 'result_too_large' },
  });
});

test('library RPC rejects malformed cursors and oversized or non-exact daemon results body-free', async () => {
  let daemonResponse;
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async () => daemonResponse,
  });
  for (const params of [{}, { cursor: 'bad' }, { cursor: null, extra: true }]) {
    const rejected = await router(Buffer.from(JSON.stringify({
      id: 'rpc_bad_cursor', method: 'memex.library.list', params,
    })));
    assert.equal(rejected.error.code, 'invalid_request');
  }

  daemonResponse = {
    ok: true,
    library_items: Array.from({ length: 51 }, (_, index) => ({
      item_id: `item_${index}`, revision_id: `revision_${index}`, title: 'Fixture',
      item_type: 'webpage', captured_at: null,
    })),
    next_cursor: null,
  };
  const oversizedPage = await router(Buffer.from(JSON.stringify({
    id: 'rpc_large_page', method: 'memex.library.list', params: { cursor: null },
  })));
  assert.deepEqual(oversizedPage, {
    id: 'rpc_large_page', error: { code: 'internal_error', message: 'internal_error' },
  });

  daemonResponse = {
    ok: true,
    library_items: [{
      item_id: 'item_fixture', revision_id: 'revision_fixture', title: 'Fixture',
      item_type: 'webpage', captured_at: null, reflected_url: 'https://private.invalid/',
    }],
    next_cursor: null,
  };
  const nonExact = await router(Buffer.from(JSON.stringify({
    id: 'rpc_non_exact', method: 'memex.library.list', params: { cursor: null },
  })));
  assert.deepEqual(nonExact, {
    id: 'rpc_non_exact', error: { code: 'internal_error', message: 'internal_error' },
  });
  assert.equal(JSON.stringify(nonExact).includes('private.invalid'), false);

  daemonResponse = {
    ok: true,
    library_items: [{
      item_id: 'item_fixture', revision_id: 'revision_fixture', title: 'Fixture',
      item_type: 'webpage', captured_at: '2026-01-01',
    }],
    next_cursor: null,
  };
  const invalidDate = await router(Buffer.from(JSON.stringify({
    id: 'rpc_invalid_date', method: 'memex.library.list', params: { cursor: null },
  })));
  assert.equal(invalidDate.error.code, 'internal_error');

  daemonResponse = {
    ok: false,
    reason_code: 'library_scan_limit_exceeded',
    reflected_private_body: 'must-not-escape',
  };
  const boundedFailure = await router(Buffer.from(JSON.stringify({
    id: 'rpc_scan_ceiling', method: 'memex.library.list', params: { cursor: null },
  })));
  assert.deepEqual(boundedFailure, {
    id: 'rpc_scan_ceiling',
    error: {
      code: 'library_scan_limit_exceeded',
      message: 'library_scan_limit_exceeded',
    },
  });
  assert.equal(JSON.stringify(boundedFailure).includes('must-not-escape'), false);
});

test('remote authorization failures remain one body-free non-enumerating error', async () => {
  const router = createRpcRouter({
    socketPath: '/synthetic/owner.sock',
    sendCommand: async (_, command) => ({
      ok: false,
      reason_code: command.command === 'library_item_read'
        ? 'library_resource_not_found' : 'remote_resource_not_found',
      reflected: command,
    }),
  });
  for (const request of [
    {
      id: 'rpc_local_status', method: 'memex.capture.status',
      params: { job_id: 'job_00000000000000000000000000000001' },
    },
    {
      id: 'rpc_guessed_item', method: 'memex.item.read',
      params: {
        job_id: 'job_00000000000000000000000000000002',
        item_id: 'item_unrelated_guess',
      },
    },
    {
      id: 'rpc_missing_library_item', method: 'memex.library.item.read',
      params: { item_id: 'item_unrelated_guess' },
    },
  ]) {
    const response = await router(Buffer.from(JSON.stringify(request)));
    const code = request.method === 'memex.library.item.read'
      ? 'library_resource_not_found' : 'remote_resource_not_found';
    assert.deepEqual(response, {
      id: request.id,
      error: { code, message: code },
    });
  }
});
