'use strict';

const assert = require('node:assert/strict');
const { randomBytes, randomUUID, webcrypto } = require('node:crypto');
const { EventEmitter } = require('node:events');
const test = require('node:test');
const { WebSocket } = require('ws');
const { clientProtocols, deriveCapability } = require('../admission');
const { BridgeRunner, pairingPayload } = require('../mac/bridge');
const { MemoryStateStore, beginFreshPairing } = require('../mac/state-store');
const { createRelay } = require('../relay/server');

const RESET = Buffer.from('memex_peer_reset_v1', 'ascii');
const ORIGIN = 'https://synthetic-reconnect.example';

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function deadline(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`deadline: ${label}`)), 3000);
    })]);
  } finally { clearTimeout(timer); }
}

function event(socket, name, predicate = () => true) {
  return deadline(new Promise((resolve) => {
    const listener = (...args) => {
      if (!predicate(...args)) return;
      socket.off(name, listener);
      resolve(args);
    };
    socket.on(name, listener);
  }), name);
}

async function fixture(t, router) {
  const protocol = await import('../web/protocol.mjs');
  const root = randomBytes(32);
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const relay = createRelay({ port: 0, admissionRoot: root, browserOrigin: ORIGIN });
  const address = await relay.listen();
  const relayUrl = `ws://127.0.0.1:${address.port}/relay`;
  const sockets = [];
  const runners = [];
  t.after(async () => {
    for (const runner of runners) runner.stop();
    for (const socket of sockets) socket.terminate();
    await relay.close();
  });
  const startMac = async () => {
    const runner = new BridgeRunner({ relayUrl, admissionRoot: root, stateStore: store, router });
    runners.push(runner);
    runner.start(state);
    sockets.push(runner.socket);
    await event(runner.socket, 'open');
    return runner;
  };
  const runner = await startMac();
  const identity = await protocol.generateIdentity(webcrypto.subtle);
  const phoneDeviceId = randomUUID();
  let pairing = pairingPayload(state, relayUrl, root, 'browser', `${ORIGIN}/reader`);
  async function phone() {
    const socket = new WebSocket(protocol.relaySocketUrl(pairing, { allowLocal: true }),
      protocol.browserSubprotocols(pairing), { origin: ORIGIN, perMessageDeflate: false });
    sockets.push(socket);
    const ready = deferred();
    const incoming = [];
    const errors = [];
    const session = new protocol.WebSecureSession({
      pairing, identity, phoneDeviceId, subtle: webcrypto.subtle,
      send: raw => socket.send(raw),
      onTrust: trusted => { pairing = trusted; },
      onReady: () => ready.resolve(),
    });
    let chain = Promise.resolve();
    socket.on('message', raw => {
      incoming.push(JSON.parse(raw.toString()));
      chain = chain.then(() => session.receive(raw)).catch(error => {
        errors.push(error.message);
        ready.reject(error);
      });
    });
    socket.on('error', error => ready.reject(error));
    socket.on('close', () => { session.close(); ready.reject(new Error('phone closed before ready')); });
    // Attach before opening so early handshake failures never become unhandled rejections.
    const established = deadline(ready.promise, 'fresh handshake');
    await Promise.all([
      established,
      event(socket, 'open').then(() => session.start(webcrypto)),
    ]);
    return { socket, session, incoming, errors };
  }
  return { protocol, runner, startMac, phone, store, state };
}

async function request(phone, id) {
  const response = await deadline(phone.session.request(id, 'memex.library.list', { cursor: null }), id);
  assert.deepEqual(response, { id, result: { marker: id } });
  assert.deepEqual(phone.errors, []);
}

for (const [departure, outcome] of [
  ['disconnect', 'response'], ['disconnect', 'error'],
  ['replacement', 'response'], ['replacement', 'error'],
]) {
  test(`phone ${departure} fences a late owner ${outcome} without replaying its write`, async t => {
    const started = deferred();
    const finish = deferred();
    const calls = [];
    const f = await fixture(t, async raw => {
      const rpc = JSON.parse(raw.toString());
      calls.push(rpc);
      if (rpc.method === 'memex.capture.enqueue') {
        started.resolve();
        await finish.promise;
        if (outcome === 'error') throw new Error('synthetic_late_owner_error');
        return { id: rpc.id, result: { marker: 'OLD_RESPONSE_MUST_NOT_LEAK' } };
      }
      return { id: rpc.id, result: { marker: rpc.id } };
    });
    const first = await f.phone();
    const mac = f.runner.socket;
    const pending = first.session.request('old_write', 'memex.capture.enqueue', {
      request_key: 'synthetic_durable_operation', url: 'https://synthetic.example/item',
    }).catch(error => error.message);
    await deadline(started.promise, 'owner write dispatch');
    const reset = event(mac, 'ping', data => data.equals(RESET));
    const departed = event(first.socket, 'close');
    let second;
    if (departure === 'disconnect') {
      first.socket.close();
      await Promise.all([reset, departed]);
      second = await f.phone();
    } else {
      [second] = await Promise.all([f.phone(), reset]);
      assert.equal((await departed)[0], 4003);
    }
    assert.equal(await pending, 'transport_unavailable');
    assert.equal(f.runner.socket, mac);
    assert.equal(mac.readyState, WebSocket.OPEN);
    const oldHello = first.incoming[0];
    const newHello = second.incoming[0];
    assert.equal(newHello.handshake_mode, 'trusted_reconnect');
    assert.notEqual(newHello.mac_ephemeral_public_key, oldHello.mac_ephemeral_public_key);
    assert.notEqual(newHello.server_nonce, oldHello.server_nonce);
    assert.notEqual(newHello.phone_ephemeral_public_key, oldHello.phone_ephemeral_public_key);
    assert.equal(f.store.load().generation, f.state.generation);
    finish.resolve();
    // Event-loop barrier drains the old owner's promise continuations, not a timing sleep.
    await new Promise(resolve => setImmediate(resolve));
    await request(second, 'new_read');
    assert.equal(mac.readyState, WebSocket.OPEN);
    assert.deepEqual(calls.map(rpc => rpc.id), ['old_write', 'new_read']);
    // Exactly hello, ready and the new reply: no suppressed response consumes wire/counter space.
    assert.equal(second.incoming.length, 3);
  });
}

test('old-key ciphertext with the expected counter cannot dispatch after a fresh handshake', async t => {
  const calls = [];
  const f = await fixture(t, async raw => { calls.push(raw); return {}; });
  const first = await f.phone();
  const ciphertext = await f.protocol.encryptEnvelope({
    key: first.session.keys.phoneToMac, sessionId: f.state.session_id, sender: 'iphone',
    counter: 0n, plaintext: JSON.stringify({ id: 'stale', method: 'memex.library.list', params: {} }),
    subtle: webcrypto.subtle,
  });
  const reset = event(f.runner.socket, 'ping', data => data.equals(RESET));
  first.socket.close();
  await reset;
  const second = await f.phone();
  const macClosed = event(f.runner.socket, 'close');
  const phoneClosed = event(second.socket, 'close');
  second.socket.send(JSON.stringify(ciphertext));
  assert.equal((await macClosed)[0], 4000);
  assert.equal((await phoneClosed)[0], 4002);
  assert.deepEqual(calls, []);
});

test('Mac replacement closes the old phone with 4005 and leaves the new Mac usable', async t => {
  const calls = [];
  const f = await fixture(t, async raw => {
    const rpc = JSON.parse(raw.toString());
    calls.push(rpc.id);
    return { id: rpc.id, result: { marker: rpc.id } };
  });
  const first = await f.phone();
  const oldMac = f.runner.socket;
  const oldClosed = event(oldMac, 'close');
  const phoneClosed = event(first.socket, 'close');
  oldMac.once('close', () => f.runner.stop()); // Do not let the intentionally replaced runner reconnect.
  const replacement = await f.startMac();
  assert.equal((await oldClosed)[0], 4003);
  assert.equal((await phoneClosed)[0], 4005);
  const second = await f.phone();
  await request(second, 'replacement_read');
  assert.equal(replacement.socket.readyState, WebSocket.OPEN);
  assert.deepEqual(calls, ['replacement_read']);
});

test('phone-generated reset Ping is answered locally, never forwarded as Mac authority', async t => {
  const f = await fixture(t, async raw => {
    const rpc = JSON.parse(raw.toString());
    return { id: rpc.id, result: { marker: rpc.id } };
  });
  const phone = await f.phone();
  const macPings = [];
  f.runner.socket.on('ping', data => macPings.push(data.toString()));
  const pong = event(phone.socket, 'pong', data => data.equals(RESET));
  phone.socket.ping(RESET);
  await pong;
  await request(phone, 'after_fake_reset');
  assert.equal(macPings.includes(RESET.toString()), false);
});

test('synchronous reset fences a previously queued hello before the new hello runs', async t => {
  class FakeWebSocket extends EventEmitter {
    constructor() { super(); this.readyState = WebSocket.OPEN; this.sent = []; }
    send(raw) { this.sent.push(JSON.parse(raw)); }
    close(code) { this.readyState = WebSocket.CLOSED; this.emit('close', code, Buffer.alloc(0)); }
    terminate() { this.close(1006); }
  }
  const protocol = await import('../web/protocol.mjs');
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const runner = new BridgeRunner({
    relayUrl: 'ws://127.0.0.1:1/relay', admissionRoot: randomBytes(32), stateStore: store,
    WebSocketClass: FakeWebSocket, router: async () => assert.fail('hello must not dispatch'),
  });
  t.after(() => runner.stop());
  runner.start(state);
  const socket = runner.socket;
  const hello = {
    kind: 'client_hello', protocol_version: 1, session_id: state.session_id,
    pairing_generation: state.generation, handshake_mode: 'qr_bootstrap', phone_device_id: randomUUID(),
    phone_identity_public_key: (await protocol.generateIdentity(webcrypto.subtle)).publicKey,
    phone_ephemeral_public_key: protocol.base64url((await protocol.generateEphemeral(webcrypto.subtle)).publicKeyRaw),
    client_nonce: randomBytes(32).toString('base64url'),
  };
  socket.emit('message', Buffer.from(JSON.stringify(hello)), false);
  socket.emit('ping', RESET);
  const newHello = { ...hello, client_nonce: randomBytes(32).toString('base64url') };
  socket.emit('message', Buffer.from(JSON.stringify(newHello)), false);
  await deadline(new Promise(resolve => setImmediate(resolve)), 'queued hello barrier');
  assert.equal(socket.readyState, WebSocket.OPEN);
  assert.deepEqual(socket.sent.map(message => [message.kind, message.client_nonce]),
    [['server_hello', newHello.client_nonce]]);
});

test('16 executing owner requests survive resets: the 17th cannot dispatch until an old request finishes', async t => {
  const calls = [];
  const gates = Array.from({ length: 16 }, () => deferred());
  const batches = [deferred(), deferred()];
  t.after(() => gates.forEach(gate => gate.resolve()));
  const f = await fixture(t, async raw => {
    const rpc = JSON.parse(raw.toString());
    calls.push(rpc.id);
    if (rpc.id.startsWith('held_')) {
      const index = Number(rpc.id.slice(5));
      if (calls.length === 8) batches[0].resolve();
      if (calls.length === 16) batches[1].resolve();
      await gates[index].promise;
    }
    return { id: rpc.id, result: { marker: rpc.id } };
  });
  const originalMac = f.runner.socket;
  // Browser sessions allow eight requests each. Keep both batches executing
  // across two genuine phone departures rather than changing the client's cap.
  for (let batch = 0; batch < 2; batch += 1) {
    const phone = await f.phone();
    const pending = Array.from({ length: 8 }, (_, i) => phone.session.request(
      `held_${batch * 8 + i}`, 'memex.capture.enqueue', { request_key: `synthetic_${batch * 8 + i}` },
    ).catch(error => error.message));
    await deadline(batches[batch].promise, `batch ${batch} dispatched`);
    const reset = event(originalMac, 'ping', data => data.equals(RESET));
    const closed = event(phone.socket, 'close');
    phone.socket.close();
    await Promise.all([reset, closed]);
    assert.deepEqual(await Promise.all(pending), Array(8).fill('transport_unavailable'));
  }
  const saturated = await f.phone();
  assert.equal(f.runner.socket, originalMac);
  const macClosed = event(originalMac, 'close');
  const phoneClosed = event(saturated.socket, 'close');
  const denied = saturated.session.request('seventeenth', 'memex.library.list', { cursor: null })
    .catch(error => error.message);
  const [macClose, phoneClose] = await Promise.all([macClosed, phoneClosed]);
  assert.equal(macClose[0], 4000);
  assert.equal(phoneClose[0], 4002);
  assert.equal(await denied, 'transport_unavailable');
  assert.deepEqual(calls, Array.from({ length: 16 }, (_, i) => `held_${i}`));

  // Only one old completion is necessary; the other fifteen remain executing.
  gates[0].resolve();
  await deadline(new Promise(resolve => setImmediate(resolve)), 'old owner completion');
  f.runner.reconnectForStateChange();
  await event(f.runner.socket, 'open');
  const recovered = await f.phone();
  await request(recovered, 'after_budget_release');
  assert.deepEqual(calls, [...Array.from({ length: 16 }, (_, i) => `held_${i}`), 'after_budget_release']);
  assert.equal(recovered.incoming.length, 3);
});

// Relay-only peers deliberately withhold a reset acknowledgement. No encryption
// authority is inferred from these synthetic transport frames.
async function resetBarrierFixture(t) {
  let now = 100_000;
  const root = randomBytes(32);
  const sessionId = randomBytes(32).toString('base64url');
  const relay = createRelay({
    port: 0, admissionRoot: root, browserOrigin: ORIGIN, now: () => now,
  });
  const address = await relay.listen();
  const sockets = [];
  t.after(async () => {
    for (const socket of sockets) socket.terminate();
    await relay.close();
  });
  async function open(role, autoPong = true) {
    const mode = role === 'mac' ? 'bridge' : 'browser';
    const socket = new WebSocket(`ws://127.0.0.1:${address.port}/relay/${sessionId}`,
      clientProtocols(role, mode, deriveCapability(root, sessionId, role, mode)), {
        autoPong, perMessageDeflate: false, ...(role === 'iphone' ? { origin: ORIGIN } : {}),
      });
    sockets.push(socket);
    const messages = [];
    socket.on('message', data => messages.push(data.toString()));
    await event(socket, 'open');
    return { socket, messages };
  }
  return { open, advance(ms) { now += ms; relay.sweep(); } };
}

async function wireBarrier(socket) {
  const marker = randomBytes(8);
  const pong = event(socket, 'pong', data => data.equals(marker));
  socket.ping(marker);
  await pong;
}

test('consecutive phone replacements acknowledge each reset before forwarding Mac responses', async t => {
  const f = await resetBarrierFixture(t);
  const mac = await f.open('mac');
  let resets = 0;
  mac.socket.on('ping', data => {
    if (data.equals(RESET)) mac.socket.send(`after_reset_${++resets}`);
  });
  let previous;
  // More than the pending-reset cap: correct automatic Pong ordering must drain it.
  for (let i = 1; i <= 10; i += 1) {
    const previousClosed = previous && event(previous.socket, 'close');
    const phone = await f.open('iphone');
    if (previousClosed) assert.equal((await previousClosed)[0], 4003);
    if (phone.messages.length === 0) await event(phone.socket, 'message');
    assert.deepEqual(phone.messages, [`after_reset_${i}`]);
    f.advance(3001);
    await wireBarrier(mac.socket);
    assert.equal(mac.socket.readyState, WebSocket.OPEN);
    assert.equal(phone.socket.readyState, WebSocket.OPEN);
    previous = phone;
  }
  assert.equal(resets, 10, 'late superseded-phone close must not queue an extra reset');
});

test('ordinary heartbeat Pongs cannot clear a missing exact reset echo or prevent its bounded timeout', async t => {
  const f = await resetBarrierFixture(t);
  const mac = await f.open('mac', false);
  const reset = event(mac.socket, 'ping', data => data.equals(RESET));
  const phone = await f.open('iphone');
  await reset;
  for (let i = 0; i < 3; i += 1) {
    mac.socket.pong(Buffer.alloc(0));
    mac.socket.send('must_not_cross_unacknowledged_reset');
    await wireBarrier(mac.socket); // Relay processed the preceding ordinary Pong and data.
    f.advance(999);
    assert.equal(mac.socket.readyState, WebSocket.OPEN);
  }
  assert.deepEqual(phone.messages, []);
  const macClosed = event(mac.socket, 'close');
  const phoneClosed = event(phone.socket, 'close');
  f.advance(3); // Exactly 3000ms, without waiting on wall-clock time or heartbeat expiry.
  const [macClose, phoneClose] = await Promise.all([macClosed, phoneClosed]);
  assert.equal(macClose[0], 4005);
  assert.equal(macClose[1].toString(), 'peer_reset_timeout');
  assert.equal(phoneClose[0], 4002);
  assert.deepEqual(phone.messages, []);
});

test('a late reset Pong from a superseded Mac cannot acknowledge its replacement reset', async t => {
  const f = await resetBarrierFixture(t);
  const oldMac = await f.open('mac', false);
  const oldReset = event(oldMac.socket, 'ping', data => data.equals(RESET));
  const oldPhone = await f.open('iphone');
  await oldReset;
  // Public ws pause/resume keeps the old client from acknowledging Close until
  // its late Pong has been sent. The relay still receives frames on that socket.
  oldMac.socket.pause();
  const oldClosed = event(oldMac.socket, 'close');
  const oldPhoneClosed = event(oldPhone.socket, 'close');
  const mac = await f.open('mac', false);
  assert.equal((await oldPhoneClosed)[0], 4005);
  const reset = event(mac.socket, 'ping', data => data.equals(RESET));
  const phone = await f.open('iphone');
  await reset;
  await deadline(new Promise((resolve, reject) => {
    oldMac.socket.pong(RESET, error => error ? reject(error) : resolve());
  }), 'late old Mac Pong flushed');
  oldMac.socket.resume();
  assert.equal((await oldClosed)[0], 4003);
  await wireBarrier(mac.socket);
  const macClosed = event(mac.socket, 'close');
  const phoneClosed = event(phone.socket, 'close');
  f.advance(3000);
  const [macClose, phoneClose] = await Promise.all([macClosed, phoneClosed]);
  assert.equal(macClose[0], 4005);
  assert.equal(macClose[1].toString(), 'peer_reset_timeout');
  assert.equal(phoneClose[0], 4002);
});
