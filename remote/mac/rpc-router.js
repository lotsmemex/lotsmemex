'use strict';

const { MAX_PLAINTEXT_BYTES, assertExactKeys } = require('../protocol');
const { sendOwnerCommand } = require('./ipc-client');

const METHODS = new Set([
  'memex.capture.enqueue',
  'memex.capture.status',
  'memex.item.read',
  'memex.library.list',
  'memex.library.item.read',
]);
const MAX_RPC_REQUEST_BYTES = 16 * 1024;
const MAX_RPC_RESPONSE_BYTES = MAX_PLAINTEXT_BYTES;
const MAX_LIBRARY_TRANSLATION_TEXT_BYTES = 240 * 1024;
const MAX_LIBRARY_SUMMARY_TEXT_BYTES = 8 * 1024;
const MAX_LIBRARY_PAGE_ITEMS = 50;
const ID_PATTERN = /^[A-Za-z0-9_.-]{1,128}$/;
const JOB_PATTERN = /^job_[a-f0-9]{32}$/;
const ITEM_PATTERN = /^item_[A-Za-z0-9_-]{1,128}$/;
const REQUEST_KEY_PATTERN = /^remote_[A-Za-z0-9_.-]{1,120}$/;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;

function rpcError(id, code) {
  return { id, error: { code, message: code } };
}

function parseRpcRequest(raw) {
  const bytes = Buffer.isBuffer(raw) ? raw : Buffer.from(raw, 'utf8');
  if (bytes.length === 0 || bytes.length > MAX_RPC_REQUEST_BYTES) throw new Error('invalid_request');
  let request;
  try {
    request = JSON.parse(bytes.toString('utf8'));
  } catch {
    throw new Error('invalid_request');
  }
  assertExactKeys(request, ['id', 'method', 'params'], 'invalid_request');
  if (!ID_PATTERN.test(request.id) || !METHODS.has(request.method)) throw new Error('invalid_request');
  if (!request.params || typeof request.params !== 'object' || Array.isArray(request.params)) {
    throw new Error('invalid_request');
  }
  return request;
}

function commandFor(request) {
  switch (request.method) {
    case 'memex.capture.enqueue':
      assertExactKeys(request.params, ['request_key', 'url'], 'invalid_request');
      if (
        !REQUEST_KEY_PATTERN.test(request.params.request_key)
        || typeof request.params.url !== 'string'
        || Buffer.byteLength(request.params.url) > 8 * 1024
        || !/^https?:\/\//.test(request.params.url)
      ) throw new Error('invalid_request');
      return {
        command: 'enqueue',
        producer: 'remote_iphone',
        producer_job_id: request.params.request_key,
        url: request.params.url,
      };
    case 'memex.capture.status':
      assertExactKeys(request.params, ['job_id'], 'invalid_request');
      if (!JOB_PATTERN.test(request.params.job_id)) throw new Error('invalid_request');
      return { command: 'remote_status', job_id: request.params.job_id };
    case 'memex.item.read':
      assertExactKeys(request.params, ['job_id', 'item_id'], 'invalid_request');
      if (
        !JOB_PATTERN.test(request.params.job_id)
        || !ITEM_PATTERN.test(request.params.item_id)
      ) throw new Error('invalid_request');
      return {
        command: 'remote_item_read',
        job_id: request.params.job_id,
        item_id: request.params.item_id,
      };
    case 'memex.library.list':
      assertExactKeys(request.params, ['cursor'], 'invalid_request');
      if (request.params.cursor !== null && !ITEM_PATTERN.test(request.params.cursor)) {
        throw new Error('invalid_request');
      }
      return { command: 'library_list', cursor: request.params.cursor };
    case 'memex.library.item.read':
      assertExactKeys(request.params, ['item_id'], 'invalid_request');
      if (!ITEM_PATTERN.test(request.params.item_id)) throw new Error('invalid_request');
      return { command: 'library_item_read', item_id: request.params.item_id };
    default:
      throw new Error('method_not_found');
  }
}

function boundedText(value, limit) {
  return typeof value === 'string'
    && Buffer.byteLength(value) <= limit
    && !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(value);
}

function itemResult(value, expectedItemId) {
  assertExactKeys(value, ['item_id', 'revision_id', 'title', 'markdown'], 'internal_error');
  if (
    value.item_id !== expectedItemId
    || !ITEM_PATTERN.test(value.item_id)
    || !ID_PATTERN.test(value.revision_id)
    || !boundedText(value.title, 8 * 1024)
    || !boundedText(value.markdown, MAX_RPC_RESPONSE_BYTES)
  ) throw new Error('internal_error');
  return value;
}

function libraryItemResult(value, expectedItemId) {
  assertExactKeys(
    value,
    [
      'item_id', 'revision_id', 'summary_state', 'summary',
      'translation_state', 'title', 'markdown',
    ],
    'internal_error',
  );
  const states = new Set([
    'not_requested', 'queued', 'running', 'succeeded', 'failed', 'unavailable',
  ]);
  if (
    value.item_id !== expectedItemId
    || !ITEM_PATTERN.test(value.item_id)
    || !ID_PATTERN.test(value.revision_id)
    || !states.has(value.summary_state)
    || !states.has(value.translation_state)
  ) throw new Error('internal_error');
  if (value.summary_state === 'succeeded') {
    if (typeof value.summary !== 'string') throw new Error('internal_error');
    if (!boundedText(value.summary, MAX_LIBRARY_SUMMARY_TEXT_BYTES)) {
      throw new Error('result_too_large');
    }
  } else if (value.summary !== null) {
    throw new Error('internal_error');
  }
  if (value.translation_state === 'succeeded') {
    if (typeof value.title !== 'string' || typeof value.markdown !== 'string') {
      throw new Error('internal_error');
    }
    if (!boundedText(value.title, 8 * 1024)
      || !boundedText(value.markdown, MAX_LIBRARY_TRANSLATION_TEXT_BYTES)
      || Buffer.byteLength(value.title) + Buffer.byteLength(value.markdown)
        + (value.summary === null ? 0 : Buffer.byteLength(value.summary))
        > MAX_LIBRARY_TRANSLATION_TEXT_BYTES) throw new Error('result_too_large');
  } else if (value.title !== null || value.markdown !== null) {
    throw new Error('internal_error');
  }
  return value;
}

function libraryListResult(response) {
  if (!Array.isArray(response.library_items)
    || response.library_items.length > MAX_LIBRARY_PAGE_ITEMS) throw new Error('internal_error');
  const items = response.library_items.map((item) => {
    assertExactKeys(
      item,
      ['item_id', 'revision_id', 'title', 'item_type', 'captured_at'],
      'internal_error',
    );
    if (
      !ITEM_PATTERN.test(item.item_id)
      || !ID_PATTERN.test(item.revision_id)
      || !boundedText(item.title, 4 * 1024)
      || !boundedText(item.item_type, 128)
      || (item.captured_at !== null && (
        !boundedText(item.captured_at, 64)
        || !RFC3339_PATTERN.test(item.captured_at)
        || !Number.isFinite(Date.parse(item.captured_at))
      ))
    ) throw new Error('internal_error');
    return item;
  });
  const nextCursor = response.next_cursor;
  if (nextCursor !== null && (
    !ITEM_PATTERN.test(nextCursor)
    || items.length === 0
    || items.at(-1).item_id !== nextCursor
  )) throw new Error('internal_error');
  return { items, next_cursor: nextCursor };
}

function mapDaemonResponse(request, response) {
  if (!response || typeof response !== 'object' || Array.isArray(response)) {
    throw new Error('internal_error');
  }
  if (!response.ok) {
    const allowed = new Set([
      'invalid_request', 'job_not_found', 'request_identity_conflict',
      'remote_resource_not_found', 'library_resource_not_found',
      'invalid_cursor', 'result_too_large', 'library_scan_limit_exceeded',
    ]);
    return rpcError(request.id, allowed.has(response.reason_code) ? response.reason_code : 'internal_error');
  }
  if (request.method === 'memex.library.list') {
    return { id: request.id, result: libraryListResult(response) };
  }
  let result;
  if (request.method === 'memex.item.read') {
    result = itemResult(response.item, request.params.item_id);
  } else if (request.method === 'memex.library.item.read') {
    result = libraryItemResult(response.library_item, request.params.item_id);
  } else {
    result = response.job;
  }
  if (!result || typeof result !== 'object' || Array.isArray(result)) {
    return rpcError(request.id, 'internal_error');
  }
  return { id: request.id, result };
}

function createRpcRouter({ socketPath, sendCommand = sendOwnerCommand }) {
  if (typeof socketPath !== 'string' || socketPath.length === 0) throw new Error('invalid_socket_path');
  return async (raw) => {
    let request;
    try {
      request = parseRpcRequest(raw);
      const command = commandFor(request);
      const response = mapDaemonResponse(request, await sendCommand(socketPath, command));
      if (Buffer.byteLength(JSON.stringify(response)) > MAX_RPC_RESPONSE_BYTES) {
        return rpcError(request.id, 'result_too_large');
      }
      return response;
    } catch (error) {
      const id = request?.id && ID_PATTERN.test(request.id) ? request.id : null;
      const allowed = new Set([
        'invalid_request', 'method_not_found', 'daemon_unavailable', 'result_too_large',
      ]);
      const code = allowed.has(error.message) ? error.message : 'internal_error';
      return rpcError(id, code);
    }
  };
}

module.exports = {
  MAX_RPC_REQUEST_BYTES,
  MAX_RPC_RESPONSE_BYTES,
  MAX_LIBRARY_PAGE_ITEMS,
  METHODS,
  commandFor,
  createRpcRouter,
  parseRpcRequest,
};
