'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { randomBytes, webcrypto } = require('node:crypto');
const test = require('node:test');
const { WebSocket } = require('ws');
const { clientProtocols, deriveCapability } = require('../admission');
const { PairingController } = require('../mac/bridge-control');
const { browserPairingCode } = require('../mac/bridge');
const { createRpcRouter } = require('../mac/rpc-router');
const { MacSecureSession } = require('../mac/secure-session');
const { MemoryStateStore, pendingPairingState } = require('../mac/state-store');
const { createRelay } = require('../relay/server');

const JOB_ID = 'job_00000000000000000000000000000001';
const ITEM_ID = 'item_offline_fixture';

function open(url, protocols) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, protocols, { perMessageDeflate: false });
    socket.once('open', () => resolve(socket));
    socket.once('error', reject);
  });
}

function fakeOwnerDaemon(socketPath, commands) {
  let stored = false;
  let approved = false;
  let translated = false;
  let summarized = false;
  const server = net.createServer((socket) => {
    let buffer = Buffer.alloc(0);
    socket.on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length < 4) return;
      const length = buffer.readUInt32BE();
      if (buffer.length !== length + 4) return;
      const command = JSON.parse(buffer.subarray(4).toString('utf8'));
      commands.push(command);
      let response;
      if (command.command === 'enqueue') {
        response = {
          ok: true,
          job: { job_id: JOB_ID, item_id: null, state: 'queued', stage: 'extract' },
          item: null,
        };
      } else if (command.command === 'remote_status') {
        response = {
          ok: true,
          job: approved
            ? { job_id: JOB_ID, item_id: ITEM_ID, state: 'succeeded', stage: 'commit' }
            : { job_id: JOB_ID, item_id: null, state: 'needs_user', stage: 'validate' },
          item: null,
        };
      } else if (command.command === 'library_list') {
        response = {
          ok: true,
          library_items: stored ? [{
            item_id: ITEM_ID,
            revision_id: 'revision_offline_fixture',
            title: 'Offline synthetic result',
            item_type: 'webpage',
            captured_at: '2026-01-01T00:00:00Z',
          }] : [],
          next_cursor: null,
        };
      } else if (command.command === 'remote_item_read') {
        response = {
          ok: true,
          job: null,
          item: {
            item_id: ITEM_ID,
            revision_id: 'rev_offline_fixture',
            title: 'Offline synthetic result',
            markdown: '# Synthetic stored item\n\nNo network or provider was used.',
          },
        };
      } else if (command.command === 'library_item_read') {
        response = {
          ok: true,
          library_item: {
            item_id: ITEM_ID,
            revision_id: 'rev_offline_fixture',
            summary_state: summarized ? 'succeeded' : 'queued',
            summary: summarized ? '这是经过校验的合成摘要。' : null,
            translation_state: translated ? 'succeeded' : 'queued',
            title: translated ? '离线合成翻译' : null,
            markdown: translated ? '# 已校验的翻译\n\n这是无私密数据的合成结果。' : null,
          },
        };
      } else {
        response = { ok: false, reason_code: 'invalid_request' };
      }
      const body = Buffer.from(JSON.stringify(response), 'utf8');
      const frame = Buffer.alloc(4 + body.length);
      frame.writeUInt32BE(body.length);
      body.copy(frame, 4);
      socket.end(frame);
    });
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, () => resolve({
      server,
      approve() { approved = true; stored = true; },
      completeTranslation() { translated = true; },
      completeSummary() { summarized = true; },
    }));
  });
}

test('offline encrypted owner approval, Store success, generated analysis, and live read are distinct', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-remote-e2e-'));
  const socketPath = path.join(root, 'owner.sock');
  const commands = [];
  const daemon = await fakeOwnerDaemon(socketPath, commands);
  const admissionRoot = randomBytes(32);
  const browserOrigin = 'https://offline-browser.example';
  const relay = createRelay({
    port: 0, heartbeatMs: 2_000, admissionRoot, browserOrigin,
  });
  const address = await relay.listen();
  const logs = [];
  const originalLog = console.log;
  const originalError = console.error;
  console.log = (...values) => logs.push(values.join(' '));
  console.error = (...values) => logs.push(values.join(' '));

  try {
    const stateStore = new MemoryStateStore();
    const webClientUrl = `${browserOrigin}/app`;
    const relayUrl = `ws://127.0.0.1:${address.port}/relay`;
    let pairingCode;
    let pairingPresentationClosed = 0;
    const pairingController = new PairingController({
      stateStore,
      createPairingCode: (state) => browserPairingCode(
        state, relayUrl, admissionRoot, webClientUrl, { allowLocal: true },
      ),
      presentPairingCode: async (code) => {
        pairingCode = code;
        return () => { pairingPresentationClosed += 1; };
      },
      startPendingBridge: () => {},
      stopPendingBridge: () => {},
      replaceActiveBridge: () => {},
    });
    pairingController.initialize();
    assert.deepEqual(await pairingController.generatePairing(), { ok: true, state: 'ready' });
    const state = pendingPairingState(stateStore.load());
    const base = `ws://127.0.0.1:${address.port}/relay/${state.session_id}`;
    const mac = await open(base, clientProtocols(
      'mac', 'bridge', deriveCapability(admissionRoot, state.session_id, 'mac', 'bridge'),
    ));
    const router = createRpcRouter({ socketPath });
    const macSession = new MacSecureSession({
      stateStore,
      router,
      send: (message) => mac.send(JSON.stringify(message)),
      onPairingConsumed: () => pairingController.pairingConsumed(),
    });
    let macChain = Promise.resolve();
    mac.on('message', (data) => { macChain = macChain.then(() => macSession.receive(data)); });

    const browserProtocol = await import('../web/protocol.mjs');
    const { openSecureChannel } = await import('../web/app.mjs');
    const phoneIdentity = await browserProtocol.generateIdentity(webcrypto.subtle);
    const scannedPairing = browserProtocol.parsePairingCode(
      pairingCode,
      webClientUrl,
      { webClientUrl, relayUrl, allowLocal: true },
    );
    let trustedPairing;
    class SyntheticBrowserSocket extends WebSocket {
      constructor(url, protocols) {
        super(url, protocols, { headers: { origin: browserOrigin }, perMessageDeflate: false });
      }
    }
    const channel = await openSecureChannel({
      pairing: scannedPairing,
      identity: phoneIdentity,
      phoneId: '33333333-3333-4333-8333-333333333333',
      persistTrust: async (pairing) => { trustedPairing = pairing; },
      WebSocketValue: SyntheticBrowserSocket,
      allowLocal: true,
    });
    assert.equal(trustedPairing.bootstrap_secret, null);
    assert.equal(trustedPairing.expires_at_ms, 0);
    assert.equal(pairingController.status(), 'paired');
    assert.equal(pairingPresentationClosed, 1);

    const requestKey = 'remote_stable_offline_fixture';
    const enqueue = await channel.request('rpc_enqueue', 'memex.capture.enqueue', {
      request_key: requestKey, url: 'https://public.example/synthetic-article',
    });
    assert.equal(enqueue.result.state, 'queued');
    const status = await channel.request(
      'rpc_status', 'memex.capture.status', { job_id: JOB_ID },
    );
    assert.equal(status.result.state, 'needs_user');
    assert.equal(status.result.item_id, null);
    daemon.approve();
    const succeeded = await channel.request(
      'rpc_status_after_approval', 'memex.capture.status', { job_id: JOB_ID },
    );
    assert.equal(succeeded.result.state, 'succeeded');
    assert.equal(succeeded.result.item_id, ITEM_ID);
    const item = await channel.request(
      'rpc_read', 'memex.item.read', { job_id: JOB_ID, item_id: ITEM_ID },
    );
    assert.equal(item.result.title, 'Offline synthetic result');
    assert.match(item.result.markdown, /Synthetic stored item/);
    const library = await channel.request(
      'rpc_library_list', 'memex.library.list', { cursor: null },
    );
    assert.equal(library.result.items.length, 1);
    assert.equal(library.result.items[0].item_id, ITEM_ID);
    const pendingTranslation = await channel.request(
      'rpc_library_pending', 'memex.library.item.read', { item_id: ITEM_ID },
    );
    assert.deepEqual(pendingTranslation.result, {
      item_id: ITEM_ID,
      revision_id: 'rev_offline_fixture',
      summary_state: 'queued',
      summary: null,
      translation_state: 'queued',
      title: null,
      markdown: null,
    });
    daemon.completeTranslation();
    const libraryItem = await channel.request(
      'rpc_library_translated', 'memex.library.item.read', { item_id: ITEM_ID },
    );
    assert.equal(libraryItem.result.translation_state, 'succeeded');
    assert.equal(libraryItem.result.title, '离线合成翻译');
    assert.match(libraryItem.result.markdown, /已校验的翻译/);
    assert.equal(libraryItem.result.summary_state, 'queued');
    assert.equal(libraryItem.result.summary, null);
    daemon.completeSummary();
    const analyzedItem = await channel.request(
      'rpc_library_analyzed', 'memex.library.item.read', { item_id: ITEM_ID },
    );
    assert.equal(analyzedItem.result.summary_state, 'succeeded');
    assert.match(analyzedItem.result.summary, /经过校验/);
    assert.equal(analyzedItem.result.translation_state, 'succeeded');
    assert.equal(commands[0].producer, 'remote_iphone');
    assert.equal(commands[0].producer_job_id, requestKey);
    assert.deepEqual(commands.slice(-4), [
      { command: 'library_list', cursor: null },
      { command: 'library_item_read', item_id: ITEM_ID },
      { command: 'library_item_read', item_id: ITEM_ID },
      { command: 'library_item_read', item_id: ITEM_ID },
    ]);
    const serializedCommands = JSON.stringify(commands.slice(-4));
    assert.equal(serializedCommands.includes('public.example'), false);
    assert.equal(serializedCommands.includes('Synthetic stored item'), false);
    assert.equal(JSON.stringify(logs).includes('已校验的翻译'), false);
    assert.equal(JSON.stringify(logs).includes('经过校验'), false);

    channel.close();
    mac.close();
  } finally {
    console.log = originalLog;
    console.error = originalError;
    await relay.close();
    await new Promise((resolve) => daemon.server.close(resolve));
    fs.rmSync(root, { recursive: true, force: true });
  }
  assert.deepEqual(logs, []);
});
