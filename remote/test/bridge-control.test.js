'use strict';

const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const {
  BridgeControlServer,
  PairingController,
} = require('../mac/bridge-control');
const { BridgeRunner, promoteTrustedRunner } = require('../mac/bridge');
const {
  MemoryStateStore,
  beginFreshPairing,
  commitPhoneTrust,
  pendingPairingState,
  revokeTrust,
} = require('../mac/state-store');
const { generateIdentity } = require('../protocol');

function controlClient(socketPath, command) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    const body = Buffer.from(JSON.stringify({ command }), 'utf8');
    const request = Buffer.alloc(4 + body.length);
    request.writeUInt32BE(body.length);
    body.copy(request, 4);
    let response = Buffer.alloc(0);
    socket.once('connect', () => socket.write(request));
    socket.on('data', (chunk) => { response = Buffer.concat([response, chunk]); });
    socket.once('error', reject);
    socket.once('end', () => {
      try {
        assert.ok(response.length >= 4);
        const length = response.readUInt32BE(0);
        assert.equal(response.length, length + 4);
        resolve(JSON.parse(response.subarray(4).toString('utf8')));
      } catch (error) { reject(error); }
    });
  });
}

function fixtureController({ nowValue = 1_000 } = {}) {
  const stateStore = new MemoryStateStore();
  const presentations = [];
  const pairingStates = [];
  const timers = [];
  const bridgeEvents = [];
  let now = nowValue;
  const controller = new PairingController({
    stateStore,
    createPairingCode(state) {
      pairingStates.push(state);
      return `MEMEX PAIR 1:${Buffer.from(JSON.stringify({
        generation: state.generation,
        bootstrap: state.bootstrap_secret,
      })).toString('base64url')}`;
    },
    async presentPairingCode(code) {
      const presentation = { code, closed: 0 };
      presentations.push(presentation);
      return () => { presentation.closed += 1; };
    },
    startPendingBridge() { bridgeEvents.push('pending:start'); },
    stopPendingBridge() { bridgeEvents.push('pending:stop'); },
    replaceActiveBridge() { bridgeEvents.push('active:replace'); },
    now: () => now,
    setTimer(callback, delay) {
      const timer = { callback, delay, cleared: false, unref() {} };
      timers.push(timer);
      return timer;
    },
    clearTimer(timer) { timer.cleared = true; },
  });
  controller.initialize();
  return {
    controller,
    bridgeEvents,
    pairingStates,
    presentations,
    stateStore,
    timers,
    setNow(value) { now = value; },
  };
}

test('owner-only bridge control is single-flight, sanitized, and preserves running Mac identity', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-bridge-control-'));
  const root = fs.realpathSync(rawRoot);
  fs.chmodSync(root, 0o700);
  const socketPath = path.join(root, 'control.sock');
  const fixture = fixtureController();
  const identityBefore = fixture.stateStore.loadOrCreate();
  const server = new BridgeControlServer({ controller: fixture.controller, socketPath });
  await server.start();
  t.after(async () => {
    await server.close();
    fs.rmSync(rawRoot, { recursive: true, force: true });
  });

  const socketMetadata = fs.lstatSync(socketPath);
  assert.equal(socketMetadata.isSocket(), true);
  assert.equal(socketMetadata.isSymbolicLink(), false);
  assert.equal(socketMetadata.uid, process.geteuid());
  assert.equal(socketMetadata.nlink, 1);
  assert.equal(socketMetadata.mode & 0o777, 0o600);

  const status = await controlClient(socketPath, 'status');
  assert.deepEqual(status, { ok: true, state: 'unpaired' });
  const generated = await controlClient(socketPath, 'generate_pairing');
  assert.deepEqual(generated, { ok: true, state: 'ready' });
  const duplicate = await controlClient(socketPath, 'generate_pairing');
  assert.deepEqual(duplicate, {
    ok: false,
    reason_code: 'pairing_already_active',
    state: 'ready',
  });
  assert.equal(fixture.presentations.length, 1);
  assert.deepEqual(fixture.bridgeEvents, ['pending:start']);

  const cancelled = await controlClient(socketPath, 'cancel_pairing');
  assert.deepEqual(cancelled, { ok: true, state: 'unpaired' });
  assert.equal(fixture.presentations[0].closed, 1);
  assert.equal(fixture.stateStore.load().pending_pairing, null);
  const regenerated = await controlClient(socketPath, 'generate_pairing');
  assert.deepEqual(regenerated, { ok: true, state: 'ready' });

  const active = fixture.pairingStates[1];
  const durable = fixture.stateStore.load();
  assert.equal(durable.mac_device_id, identityBefore.mac_device_id);
  assert.equal(durable.mac_identity_public_key, identityBefore.mac_identity_public_key);
  assert.equal(durable.mac_identity_private_key, identityBefore.mac_identity_private_key);
  const responseText = JSON.stringify([status, generated, duplicate]);
  for (const secret of [
    active.session_id,
    active.bootstrap_secret,
    durable.mac_device_id,
    durable.mac_identity_public_key,
    durable.mac_identity_private_key,
    fixture.presentations[0].code,
    socketPath,
  ]) assert.equal(responseText.includes(secret), false);

  const phone = generateIdentity();
  commitPhoneTrust(fixture.stateStore, {
    generation: active.generation,
    sessionId: active.session_id,
    phoneDeviceId: '33333333-3333-4333-8333-333333333333',
    phoneIdentityPublicKey: phone.public_key,
    now: 2_000,
  });
  fixture.controller.pairingConsumed();
  assert.equal(fixture.presentations[1].closed, 1);
  assert.deepEqual(await controlClient(socketPath, 'status'), { ok: true, state: 'paired' });
  assert.deepEqual(await controlClient(socketPath, 'cancel_pairing'), { ok: true, state: 'paired' });
  assert.notEqual(fixture.stateStore.load().trusted_phone, null);

  assert.deepEqual(await controlClient(socketPath, 'revoke'), { ok: true, state: 'unpaired' });
  const revoked = fixture.stateStore.load();
  assert.equal(revoked.trusted_phone, null);
  assert.equal(revoked.pending_pairing, null);
  assert.equal(revoked.mac_device_id, identityBefore.mac_device_id);
  assert.equal(revoked.mac_identity_public_key, identityBefore.mac_identity_public_key);
  assert.equal(revoked.mac_identity_private_key, identityBefore.mac_identity_private_key);
  assert.deepEqual(fixture.bridgeEvents, [
    'pending:start', 'pending:stop', 'pending:start', 'active:replace',
  ]);
});

test('sanitized status stays in the running bridge instead of rereading owner Keychain state', async () => {
  const stateStore = new MemoryStateStore();
  const originalLoadOrCreate = stateStore.loadOrCreate.bind(stateStore);
  let stateReads = 0;
  stateStore.loadOrCreate = () => {
    stateReads += 1;
    return originalLoadOrCreate();
  };
  let pendingState = null;
  let replacementState = null;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: async () => () => {},
    startPendingBridge(state) { pendingState = state; },
    stopPendingBridge() {},
    replaceActiveBridge(state) { replacementState = state; },
  });
  controller.initialize();
  const initializedReads = stateReads;
  assert.equal(controller.status(), 'unpaired');
  assert.equal(controller.status(), 'unpaired');
  assert.equal(stateReads, initializedReads);

  assert.deepEqual(await controller.generatePairing(), { ok: true, state: 'ready' });
  const generatedReads = stateReads;
  assert.equal(generatedReads, initializedReads + 1);
  assert.equal(controller.status(), 'ready');
  assert.equal(controller.status(), 'ready');
  assert.equal(stateReads, generatedReads);
  assert.equal(pendingState.generation, stateStore.load().pending_pairing.generation);
  controller.revoke();
  assert.equal(replacementState.generation, stateStore.load().generation);
  controller.shutdown();
  assert.equal(stateReads, generatedReads + 1);
});

test('pairing expiry is fenced, removes the private presentation, and revokes the capability', async () => {
  const fixture = fixtureController();
  await fixture.controller.generatePairing();
  const active = fixture.pairingStates[0];
  assert.equal(fixture.timers.length, 1);
  assert.equal(fixture.timers[0].delay, 5 * 60_000);
  fixture.setNow(active.pairing_expires_at_ms);
  fixture.timers[0].callback();
  const expired = fixture.stateStore.load();
  assert.equal(fixture.controller.status(), 'expired');
  assert.equal(expired.pending_pairing, null);
  assert.equal(expired.trusted_phone, null);
  assert.equal(fixture.presentations[0].closed, 1);

  await fixture.controller.generatePairing();
  const replacement = fixture.stateStore.load();
  fixture.timers[0].callback();
  assert.equal(fixture.stateStore.load().pairing_counter, replacement.pairing_counter);
  assert.equal(fixture.controller.status(), 'ready');
  fixture.controller.revoke();
  assert.equal(fixture.controller.status(), 'unpaired');
  assert.equal(fixture.presentations[1].closed, 1);
});

test('managed timer detach failure cannot revoke an armed one-time admission', async () => {
  const stateStore = new MemoryStateStore();
  let expiryCallback;
  let now = 1_000;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: async () => () => {},
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
    now: () => now,
    setTimer(callback) {
      expiryCallback = callback;
      return { unref() { throw new Error('managed timer cannot detach'); } };
    },
    clearTimer: () => {},
  });
  controller.initialize();

  assert.deepEqual(await controller.generatePairing(), { ok: true, state: 'ready' });
  const pending = stateStore.load().pending_pairing;
  assert.notEqual(pending, null);
  now = pending.pairing_expires_at_ms;
  expiryCallback();
  assert.equal(controller.status(), 'expired');
  assert.equal(stateStore.load().pending_pairing, null);
});

test('controller generation, cancellation, expiry, shutdown, and restart preserve established trust', async () => {
  const fixture = fixtureController();
  const first = beginFreshPairing(fixture.stateStore, 1_000);
  const phone = generateIdentity();
  commitPhoneTrust(fixture.stateStore, {
    generation: first.generation,
    sessionId: first.session_id,
    phoneDeviceId: '12121212-1212-4212-8212-121212121212',
    phoneIdentityPublicKey: phone.public_key,
    now: 2_000,
  });
  fixture.controller.initialize();
  const trusted = fixture.stateStore.load();
  const exactTrust = {
    generation: trusted.generation,
    sessionId: trusted.session_id,
    phoneDeviceId: trusted.trusted_phone.phone_device_id,
    phoneIdentityPublicKey: trusted.trusted_phone.phone_identity_public_key,
  };
  const assertLive = () => assert.equal(
    fixture.stateStore.withTrustedChannel(exactTrust, () => 'live'),
    'live',
  );

  assert.equal(fixture.controller.status(), 'paired');
  assert.deepEqual(await fixture.controller.generatePairing(), { ok: true, state: 'ready' });
  assertLive();
  assert.deepEqual(fixture.controller.cancelPairing(), { ok: true, state: 'paired' });
  assertLive();

  assert.deepEqual(await fixture.controller.generatePairing(), { ok: true, state: 'ready' });
  const expiring = fixture.pairingStates.at(-1);
  fixture.setNow(expiring.pairing_expires_at_ms);
  fixture.timers.at(-1).callback();
  assert.equal(fixture.controller.status(), 'paired');
  assertLive();

  fixture.setNow(expiring.pairing_expires_at_ms + 1);
  assert.deepEqual(await fixture.controller.generatePairing(), { ok: true, state: 'ready' });
  fixture.controller.shutdown();
  assert.equal(fixture.controller.status(), 'paired');
  assertLive();

  const restarted = new PairingController({
    stateStore: fixture.stateStore,
    createPairingCode: () => assert.fail('restart must not create pairing'),
    presentPairingCode: () => assert.fail('restart must not present pairing'),
    startPendingBridge: () => assert.fail('restart must not start pending bridge'),
    stopPendingBridge: () => {},
    replaceActiveBridge: () => {},
  });
  restarted.initialize();
  assert.equal(restarted.status(), 'paired');
  assertLive();
});

test('trust consumption while the private window opens closes that window and reports paired', async () => {
  const stateStore = new MemoryStateStore();
  let resolvePresentation;
  let closed = 0;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: () => new Promise((resolve) => { resolvePresentation = resolve; }),
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
  });
  controller.initialize();
  const generation = controller.generatePairing();
  await new Promise((resolve) => setImmediate(resolve));
  const active = pendingPairingState(stateStore.load());
  const phone = generateIdentity();
  commitPhoneTrust(stateStore, {
    generation: active.generation,
    sessionId: active.session_id,
    phoneDeviceId: '44444444-4444-4444-8444-444444444444',
    phoneIdentityPublicKey: phone.public_key,
    now: Date.now(),
  });
  controller.pairingConsumed();
  resolvePresentation(() => { closed += 1; });
  assert.deepEqual(await generation, { ok: true, state: 'paired' });
  assert.equal(closed, 1);
  assert.equal(controller.status(), 'paired');
});

test('trust consumption survives a later presentation rejection', async () => {
  const stateStore = new MemoryStateStore();
  let rejectPresentation;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: () => new Promise((resolve, reject) => { rejectPresentation = reject; }),
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
  });
  controller.initialize();
  const generation = controller.generatePairing();
  await new Promise((resolve) => setImmediate(resolve));
  const active = pendingPairingState(stateStore.load());
  const phone = generateIdentity();
  commitPhoneTrust(stateStore, {
    generation: active.generation,
    sessionId: active.session_id,
    phoneDeviceId: '55555555-5555-4555-8555-555555555555',
    phoneIdentityPublicKey: phone.public_key,
    now: Date.now(),
  });
  controller.pairingConsumed();
  rejectPresentation(new Error('late presentation failure'));
  assert.deepEqual(await generation, { ok: true, state: 'paired' });
  assert.notEqual(stateStore.load().trusted_phone, null);
});

test('concurrent generation requests still present exactly one pairing window', async () => {
  const stateStore = new MemoryStateStore();
  let resolvePresentation;
  let presentations = 0;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: () => {
      presentations += 1;
      return new Promise((resolve) => { resolvePresentation = resolve; });
    },
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
  });
  controller.initialize();
  const first = controller.generatePairing();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(await controller.generatePairing(), {
    ok: false,
    reason_code: 'pairing_already_active',
    state: 'unpaired',
  });
  assert.equal(presentations, 1);
  resolvePresentation(() => {});
  assert.deepEqual(await first, { ok: true, state: 'ready' });
  controller.revoke();
});

test('generation failure reports only a bounded phase and revokes the hidden admission', async () => {
  const stateStore = new MemoryStateStore();
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: async () => { throw new Error('private diagnostic must not escape'); },
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
  });
  controller.initialize();
  assert.deepEqual(await controller.generatePairing(), {
    ok: false,
    reason_code: 'pairing_image_unavailable',
    state: 'unpaired',
  });
  const state = stateStore.load();
  assert.equal(state.pending_pairing, null);
  assert.equal(state.trusted_phone, null);
});

test('presentation timeout revokes hidden state, permits retry, and cleans a late image', async () => {
  const stateStore = new MemoryStateStore();
  let resolveFirst;
  let presentations = 0;
  let lateCleanup = 0;
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: () => {
      presentations += 1;
      if (presentations === 1) return new Promise((resolve) => { resolveFirst = resolve; });
      return Promise.resolve(() => {});
    },
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
    presentationTimeoutMs: 5,
  });
  controller.initialize();
  assert.deepEqual(await controller.generatePairing(), {
    ok: false,
    reason_code: 'pairing_image_unavailable',
    state: 'unpaired',
  });
  assert.equal(stateStore.load().pending_pairing, null);
  assert.deepEqual(await controller.generatePairing(), { ok: true, state: 'ready' });
  resolveFirst(() => { lateCleanup += 1; });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(lateCleanup, 1);
  controller.revoke();
});

test('bridge start invalidates an orphaned admission without changing Mac identity', () => {
  const fixture = fixtureController();
  const identity = fixture.stateStore.load();
  return fixture.controller.generatePairing().then(() => {
    const orphaned = fixture.stateStore.load();
    const replacement = new PairingController({
      stateStore: fixture.stateStore,
      createPairingCode: () => assert.fail('must not render an orphaned admission'),
      presentPairingCode: () => assert.fail('must not render an orphaned admission'),
      startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
    });
    replacement.initialize();
    const recovered = fixture.stateStore.load();
    assert.equal(replacement.status(), 'expired');
    assert.equal(recovered.generation, orphaned.generation);
    assert.equal(recovered.pairing_counter, orphaned.pairing_counter);
    assert.equal(recovered.pending_pairing, null);
    assert.equal(recovered.mac_device_id, identity.mac_device_id);
    assert.equal(recovered.mac_identity_private_key, identity.mac_identity_private_key);
  });
});

test('state rotation reconnects one BridgeRunner without another process lease', () => {
  class FakeWebSocket extends EventEmitter {
    static instances = [];
    static OPEN = 1;
    constructor() {
      super();
      this.readyState = 0;
      this.terminated = false;
      FakeWebSocket.instances.push(this);
    }
    close() { this.emit('close'); }
    terminate() { this.terminated = true; this.emit('close'); }
    ping() {}
    send() {}
  }
  const stateStore = new MemoryStateStore();
  const originalLoadOrCreate = stateStore.loadOrCreate.bind(stateStore);
  let stateReads = 0;
  stateStore.loadOrCreate = () => {
    stateReads += 1;
    return originalLoadOrCreate();
  };
  const runner = new BridgeRunner({
    relayUrl: 'ws://127.0.0.1/relay',
    socketPath: '/tmp/not-opened.sock',
    admissionRoot: Buffer.alloc(32, 0x44),
    stateStore,
    WebSocketClass: FakeWebSocket,
  });
  runner.start();
  assert.equal(FakeWebSocket.instances.length, 1);
  assert.equal(stateReads, 1);
  const first = FakeWebSocket.instances[0];
  const rotated = revokeTrust(stateStore);
  runner.reconnectForStateChange(rotated);
  assert.equal(first.terminated, true);
  assert.equal(FakeWebSocket.instances.length, 2);
  assert.equal(runner.socket, FakeWebSocket.instances[1]);
  assert.equal(runner.reconnectTimer, null);
  assert.equal(stateReads, 2);
  runner.stop();

  const controlSource = fs.readFileSync(path.join(__dirname, '../mac/bridge-control.js'), 'utf8');
  assert.doesNotMatch(controlSource, /acquireProcessLease|child_process|\bspawn\b|\bexec\b/);
});

test('one bridge process keeps stable trust live while a pending runner waits for replacement', () => {
  class FakeWebSocket extends EventEmitter {
    static instances = [];
    static OPEN = 1;
    constructor(url) {
      super();
      this.url = url;
      this.readyState = 0;
      FakeWebSocket.instances.push(this);
    }
    close() { this.emit('close'); }
    terminate() { this.emit('close'); }
    ping() {}
    send() {}
  }
  const store = new MemoryStateStore();
  const initial = beginFreshPairing(store, 1_000);
  const firstPhone = generateIdentity();
  commitPhoneTrust(store, {
    generation: initial.generation,
    sessionId: initial.session_id,
    phoneDeviceId: '77777777-7777-4777-8777-777777777777',
    phoneIdentityPublicKey: firstPhone.public_key,
    now: 2_000,
  });
  const stableState = store.load();
  const replacement = beginFreshPairing(store, 3_000);
  const shared = {
    relayUrl: 'ws://127.0.0.1/relay',
    socketPath: '/tmp/not-opened.sock',
    admissionRoot: Buffer.alloc(32, 0x55),
    stateStore: store,
    WebSocketClass: FakeWebSocket,
  };
  const stable = new BridgeRunner({ ...shared, connectionMode: 'stable' });
  const pending = new BridgeRunner({ ...shared, connectionMode: 'pending' });
  stable.start();
  pending.start(replacement);

  assert.equal(FakeWebSocket.instances.length, 2);
  assert.match(FakeWebSocket.instances[0].url, new RegExp(`${stableState.session_id}$`));
  assert.match(FakeWebSocket.instances[1].url, new RegExp(`${replacement.session_id}$`));
  assert.notEqual(FakeWebSocket.instances[0].url, FakeWebSocket.instances[1].url);
  assert.equal(store.load().session_id, stableState.session_id);
  assert.equal(store.load().trusted_phone.phone_device_id, '77777777-7777-4777-8777-777777777777');

  const replacementPhone = generateIdentity();
  commitPhoneTrust(store, {
    generation: replacement.generation,
    sessionId: replacement.session_id,
    phoneDeviceId: '88888888-8888-4888-8888-888888888888',
    phoneIdentityPublicKey: replacementPhone.public_key,
    now: 4_000,
  });
  stable.stop();
  pending.promotePending();
  assert.equal(pending.connectionMode, 'stable');
  assert.equal(pending.socket, FakeWebSocket.instances[1]);
  assert.equal(FakeWebSocket.instances.length, 2);
  assert.equal(store.load().session_id, replacement.session_id);
  assert.equal(store.load().trusted_phone.phone_device_id, '88888888-8888-4888-8888-888888888888');
  pending.stop();
});

test('durable trust consumption is recorded before pending runner publication', () => {
  const events = [];
  const activeRunner = {
    stop() {
      events.push('active:stop');
      throw new Error('synthetic close failure');
    },
  };
  const runner = { promotePending() { events.push('pending:promote'); } };
  const pairingController = { pairingConsumed() { events.push('trust:consumed'); } };
  const promoted = promoteTrustedRunner({
    pairingController, runner, pendingRunner: runner, activeRunner,
  });
  assert.deepEqual(events, ['trust:consumed', 'pending:promote', 'active:stop']);
  assert.equal(promoted.activeRunner, runner);
  assert.equal(promoted.pendingRunner, null);

  events.length = 0;
  const stale = promoteTrustedRunner({
    pairingController, runner, pendingRunner: null, activeRunner,
  });
  assert.deepEqual(events, []);
  assert.equal(stale.activeRunner, activeRunner);
  assert.equal(stale.pendingRunner, null);
});

test('bridge control rejects unsafe parent and stale socket paths', async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-bridge-control-unsafe-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.chmodSync(root, 0o755);
  const fixture = fixtureController();
  const unsafeParent = new BridgeControlServer({
    controller: fixture.controller,
    socketPath: path.join(root, 'control.sock'),
  });
  await assert.rejects(() => unsafeParent.start(), /unsafe_control_socket/);

  fs.chmodSync(root, 0o700);
  const target = path.join(root, 'target');
  fs.writeFileSync(target, 'not-a-socket', { mode: 0o600 });
  const socketPath = path.join(root, 'control.sock');
  fs.symlinkSync(target, socketPath);
  const unsafeSocket = new BridgeControlServer({ controller: fixture.controller, socketPath });
  await assert.rejects(() => unsafeSocket.start(), /unsafe_control_socket/);
  assert.equal(fs.lstatSync(socketPath).isSymbolicLink(), true);
});

test('control keeps the slow-input bound while allowing one bounded owner action', async (t) => {
  const rawRoot = fs.mkdtempSync('/private/tmp/memex-control-action-');
  const root = fs.realpathSync(rawRoot);
  fs.chmodSync(root, 0o700);
  const socketPath = path.join(root, 'control.sock');
  const stateStore = new MemoryStateStore();
  const controller = new PairingController({
    stateStore,
    createPairingCode: (state) => `MEMEX PAIR 1:${state.bootstrap_secret}`,
    presentPairingCode: () => new Promise((resolve) => {
      setTimeout(() => resolve(() => {}), 30);
    }),
    startPendingBridge: () => {}, stopPendingBridge: () => {}, replaceActiveBridge: () => {},
  });
  controller.initialize();
  const server = new BridgeControlServer({
    controller,
    socketPath,
    inputTimeoutMs: 5,
    actionTimeoutMs: 100,
  });
  await server.start();
  t.after(async () => {
    controller.revoke();
    await server.close();
    fs.rmSync(rawRoot, { recursive: true, force: true });
  });
  assert.deepEqual(await controlClient(socketPath, 'generate_pairing'), {
    ok: true,
    state: 'ready',
  });

  const slowSocket = net.createConnection(socketPath);
  await new Promise((resolve, reject) => {
    slowSocket.once('connect', resolve);
    slowSocket.once('error', reject);
  });
  slowSocket.write(Buffer.from([0]));
  const closed = new Promise((resolve) => slowSocket.once('close', resolve));
  await new Promise((resolve) => setTimeout(resolve, 3));
  if (!slowSocket.destroyed) slowSocket.write(Buffer.from([0]));
  await closed;
  assert.equal(slowSocket.destroyed, true);
});
