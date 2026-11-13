'use strict';

const assert = require('node:assert/strict');
const { randomBytes } = require('node:crypto');
const { once } = require('node:events');
const http = require('node:http');
const net = require('node:net');
const test = require('node:test');
const { WebSocket } = require('ws');
const { clientProtocols, deriveCapability } = require('../admission');
const { createRelay } = require('../relay/server');

function endpoint(port, session) {
  return `ws://127.0.0.1:${port}/relay/${session}`;
}

function open(url, protocols, options = {}) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, protocols, options);
    socket.once('open', () => resolve(socket));
    socket.once('error', reject);
  });
}

function openRole(port, root, session, role, options) {
  const clientMode = role === 'mac' ? 'bridge' : 'native';
  const capability = deriveCapability(root, session, role, clientMode);
  return open(endpoint(port, session), clientProtocols(role, clientMode, capability), options);
}

function rejected(url, protocols, options = {}) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, protocols, options);
    socket.once('unexpected-response', (_, response) => {
      response.resume();
      resolve(response.statusCode);
    });
    socket.once('open', () => reject(new Error('unexpected_open')));
    socket.once('error', () => {});
  });
}

function closeCode(socket) {
  return new Promise((resolve) => socket.once('close', (code) => resolve(code)));
}

test('shutdown closes connected peers and browser preconnects without client cooperation', async () => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  // Browsers may open a TCP connection before sending any HTTP headers.
  const preconnect = net.connect({ host: '127.0.0.1', port });
  await once(preconnect, 'connect');
  preconnect.on('error', () => {}); // Forced shutdown may reset this socket.
  // A later completed HTTP request lets the server accept the earlier socket
  // before close stops listening; otherwise the test can race with accept().
  await new Promise((resolve, reject) => {
    http.get({ host: '127.0.0.1', port, path: '/healthz', agent: false }, (response) => {
      response.resume();
      response.once('end', resolve);
    }).once('error', reject);
  });
  const macClosed = closeCode(mac);
  const phoneClosed = closeCode(phone);
  const closing = relay.close();
  let timer;
  try {
    await Promise.race([closing, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('shutdown_waited_for_browser')), 1000);
    })]);
    await Promise.all([macClosed, phoneClosed]);
    assert.equal(relay.address(), null);
  } finally {
    clearTimeout(timer);
    preconnect.destroy();
    mac.terminate();
    phone.terminate();
    await closing;
  }
});

test('admitted peers get only fixed protocol, Mac-offline truth, and opaque forwarding', async () => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, heartbeatMs: 1_000, admissionRoot: root });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const offlinePhone = await openRole(port, root, session, 'iphone');
  assert.equal(offlinePhone.protocol, 'memex.remote.v1');
  assert.equal(await closeCode(offlinePhone), 4002);

  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  assert.equal(mac.protocol, 'memex.remote.v1');
  assert.equal(phone.protocol, 'memex.remote.v1');
  const opaque = JSON.stringify({ kind: 'encrypted_envelope', ciphertext: 'opaque-fixture' });
  const received = new Promise((resolve) => mac.once('message', (data) => resolve(data.toString())));
  phone.send(opaque);
  assert.equal(await received, opaque);
  const reset = once(mac, 'ping');
  const phoneClosed = closeCode(phone);
  phone.close();
  await phoneClosed;
  assert.equal((await reset)[0].toString(), 'memex_peer_reset_v1');
  assert.equal(mac.readyState, WebSocket.OPEN);

  const phone2 = await openRole(port, root, session, 'iphone');
  const receivedAfterReconnect = new Promise((resolve) => {
    mac.once('message', (data) => resolve(data.toString()));
  });
  phone2.send(opaque);
  assert.equal(await receivedAfterReconnect, opaque);
  mac.close();
  phone2.close();
  await relay.close();
});

test('Mac replacement keeps new Mac and resets phone; phone replacement keeps Mac', async () => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  const macClosed = closeCode(mac);
  const phoneClosed = closeCode(phone);
  const replacement = await openRole(port, root, session, 'mac');
  assert.equal(await macClosed, 4003);
  assert.equal(await phoneClosed, 4005);
  assert.equal(replacement.readyState, WebSocket.OPEN);

  const nextPhone = await openRole(port, root, session, 'iphone');
  const delivered = new Promise((resolve) => replacement.once('message', resolve));
  nextPhone.send('after-cleanup');
  assert.equal((await delivered).toString(), 'after-cleanup');
  const oldPhoneClosed = closeCode(nextPhone);
  const finalPhone = await openRole(port, root, session, 'iphone');
  assert.equal(await oldPhoneClosed, 4003);
  const afterOldClose = once(replacement, 'message');
  finalPhone.send('after-old-close');
  assert.equal((await afterOldClose)[0].toString(), 'after-old-close');
  assert.equal(replacement.readyState, WebSocket.OPEN);
  replacement.close();
  finalPhone.close();
  await relay.close();
});

test('Upgrade rejects missing, wrong-role, query, and wrong capabilities before room allocation', async () => {
  const root = randomBytes(32);
  const relay = createRelay({
    port: 0,
    admissionRoot: root,
    limits: { rooms: 1 },
  });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  assert.equal(await rejected(endpoint(port, session), ['memex.remote.v1']), 401);
  const wrong = deriveCapability(root, session, 'iphone', 'native');
  assert.equal(await rejected(endpoint(port, session), clientProtocols('mac', 'bridge', wrong)), 401);
  const macProtocols = clientProtocols(
    'mac', 'bridge', deriveCapability(root, session, 'mac', 'bridge'),
  );
  assert.equal(await rejected(`${endpoint(port, session)}?cap=forbidden`, macProtocols), 400);
  const otherSession = randomBytes(32).toString('base64url');
  assert.equal(await rejected(endpoint(port, otherSession), macProtocols), 401);

  const legitimate = await openRole(port, root, otherSession, 'mac');
  assert.equal(legitimate.protocol, 'memex.remote.v1');
  legitimate.close();
  await relay.close();
});

test('authenticated browser mode requires the configured exact HTTPS Origin while native stays originless', async () => {
  const root = randomBytes(32);
  const browserOrigin = 'https://review-origin.example';
  const relay = createRelay({ port: 0, admissionRoot: root, browserOrigin });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const browserCapability = deriveCapability(root, session, 'iphone', 'browser');
  const browserProtocols = clientProtocols('iphone', 'browser', browserCapability);
  assert.equal(await rejected(endpoint(port, session), browserProtocols), 403);
  assert.equal(await rejected(endpoint(port, session), browserProtocols, {
    headers: { origin: 'https://wrong-origin.example' },
  }), 403);

  const nativeCapability = deriveCapability(root, session, 'iphone', 'native');
  assert.equal(await rejected(
    endpoint(port, session),
    clientProtocols('iphone', 'browser', nativeCapability),
    { headers: { origin: browserOrigin } },
  ), 401);
  assert.equal(await rejected(
    endpoint(port, session),
    clientProtocols('iphone', 'native', nativeCapability),
    { headers: { origin: browserOrigin } },
  ), 403);

  const mac = await openRole(port, root, session, 'mac');
  const browser = await open(endpoint(port, session), browserProtocols, {
    headers: { origin: browserOrigin },
  });
  assert.equal(browser.protocol, 'memex.remote.v1');
  mac.close();
  browser.close();
  await relay.close();

  assert.throws(() => createRelay({
    port: 0, admissionRoot: root, browserOrigin: 'https://review-origin.example/path',
  }), /invalid_browser_origin/);
  for (const invalid of [
    'http://review-origin.example', 'https://review-origin.example/',
    'https://user@review-origin.example', 'https://review-origin.example?x=1',
  ]) assert.throws(() => createRelay({
    port: 0, admissionRoot: root, browserOrigin: invalid,
  }), /invalid_browser_origin/);
});

test('strict connection, room, source, frame, byte, idle, and backpressure bounds close cleanly', async () => {
  const root = randomBytes(32);
  let clock = 10_000;
  const relay = createRelay({
    port: 0,
    heartbeatMs: 60_000,
    admissionRoot: root,
    now: () => clock,
    limits: {
      totalConnections: 2,
      connectionsPerSource: 2,
      rooms: 1,
      framesPerSecond: 1,
      bytesPerSecond: 64,
      bufferedBytes: 64,
      idleRoomMs: 100,
    },
  });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  const other = randomBytes(32).toString('base64url');
  assert.equal(await rejected(
    endpoint(port, other),
    clientProtocols('mac', 'bridge', deriveCapability(root, other, 'mac', 'bridge')),
    { headers: { 'x-forwarded-for': '198.51.100.99' } },
  ), 429);

  const first = new Promise((resolve) => mac.once('message', resolve));
  phone.send('first');
  await first;
  const rateClosed = closeCode(phone);
  const rateReset = once(mac, 'ping');
  phone.send('second');
  assert.equal(await rateClosed, 4008);
  await rateReset;
  assert.equal(mac.readyState, WebSocket.OPEN);

  const idleMac = mac;
  const idlePhone = await openRole(port, root, session, 'iphone');
  // Heartbeat replies keep a quiet but live room open.
  clock += 90;
  const ponged = new Promise((resolve) => setTimeout(resolve, 50));
  idleMac.pong();
  await ponged;
  clock += 20;
  relay.sweep();
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.equal(idleMac.readyState, WebSocket.OPEN);
  assert.equal(idlePhone.readyState, WebSocket.OPEN);
  const idleMacClosed = closeCode(idleMac);
  const idlePhoneClosed = closeCode(idlePhone);
  clock += 101;
  relay.sweep();
  assert.equal(await idleMacClosed, 4009);
  assert.equal(await idlePhoneClosed, 4009);
  await relay.close();
});

test('peer buffering cap closes both live sockets before forwarding', async () => {
  const root = randomBytes(32);
  const relay = createRelay({
    port: 0,
    admissionRoot: root,
    limits: { bufferedBytes: 64 },
  });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  const macClosed = closeCode(mac);
  const phoneClosed = closeCode(phone);
  phone.send('x'.repeat(65));
  assert.equal(await macClosed, 4008);
  assert.equal(await phoneClosed, 4005);
  await relay.close();
});

test('total connections, per-source connections, and rooms reject independently before allocation', async () => {
  const cases = [
    {
      limits: { totalConnections: 1, connectionsPerSource: 10, rooms: 10 },
      name: 'total',
    },
    {
      limits: { totalConnections: 10, connectionsPerSource: 1, rooms: 10 },
      name: 'source',
    },
    {
      limits: { totalConnections: 10, connectionsPerSource: 10, rooms: 1 },
      name: 'rooms',
    },
  ];
  for (const entry of cases) {
    const root = randomBytes(32);
    const relay = createRelay({ port: 0, admissionRoot: root, limits: entry.limits });
    const { port } = await relay.listen();
    const firstSession = randomBytes(32).toString('base64url');
    const first = await openRole(port, root, firstSession, 'mac');
    const secondSession = randomBytes(32).toString('base64url');
    const status = await rejected(
      endpoint(port, secondSession),
      clientProtocols(
        'mac', 'bridge', deriveCapability(root, secondSession, 'mac', 'bridge'),
      ),
    );
    assert.equal(status, 429, entry.name);
    first.close();
    await relay.close();
  }
});

test('byte rate closes independently of frame rate', async () => {
  const root = randomBytes(32);
  const relay = createRelay({
    port: 0,
    admissionRoot: root,
    limits: { framesPerSecond: 10, bytesPerSecond: 5 },
  });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const phone = await openRole(port, root, session, 'iphone');
  const phoneClosed = closeCode(phone);
  const reset = once(mac, 'ping');
  phone.send('123456');
  assert.equal(await phoneClosed, 4008);
  await reset;
  assert.equal(mac.readyState, WebSocket.OPEN);
  await relay.close();
});

test('forwarded source headers are ignored unless the proxy is explicitly trusted', async () => {
  const root = randomBytes(32);
  const untrusted = createRelay({
    port: 0,
    admissionRoot: root,
    limits: { connectionsPerSource: 1 },
  });
  let { port } = await untrusted.listen();
  const firstSession = randomBytes(32).toString('base64url');
  const first = await openRole(port, root, firstSession, 'mac', {
    headers: { 'x-forwarded-for': '198.51.100.1' },
  });
  const secondSession = randomBytes(32).toString('base64url');
  assert.equal(await rejected(
    endpoint(port, secondSession),
    clientProtocols(
      'mac', 'bridge', deriveCapability(root, secondSession, 'mac', 'bridge'),
    ),
    { headers: { 'x-forwarded-for': '198.51.100.2' } },
  ), 429);
  first.close();
  await untrusted.close();

  const trusted = createRelay({
    port: 0,
    admissionRoot: root,
    trustProxy: true,
    limits: { connectionsPerSource: 1 },
  });
  ({ port } = await trusted.listen());
  const one = await openRole(port, root, firstSession, 'mac', {
    headers: { 'x-forwarded-for': '198.51.100.1' },
  });
  const two = await openRole(port, root, secondSession, 'mac', {
    headers: { 'x-forwarded-for': '198.51.100.2' },
  });
  one.close();
  two.close();
  await trusted.close();
});

test('local Mac ingress is distinct from proxy ingress and retains admission and shared limits', async (t) => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root, trustProxy: true,
    allowLoopbackMac: true, limits: { connectionsPerSource: 1 } });
  t.after(() => relay.close());
  const { port } = await relay.listen();
  const localPort = relay.localMacAddress().port;
  assert.notEqual(localPort, port);
  assert.equal(relay.localMacAddress().address, '127.0.0.1');
  const session = randomBytes(32).toString('base64url');
  const macProtocols = clientProtocols('mac', 'bridge', deriveCapability(root, session, 'mac', 'bridge'));
  const phoneProtocols = clientProtocols('iphone', 'native', deriveCapability(root, session, 'iphone', 'native'));
  assert.equal(await rejected(endpoint(port, session), macProtocols), 400);
  assert.equal(await rejected(endpoint(port, session), macProtocols, { headers: { host: `127.0.0.1:${localPort}` } }), 400);
  assert.equal(await rejected(endpoint(localPort, session), phoneProtocols), 403);
  assert.equal(await rejected(endpoint(localPort, session), ['memex.remote.v1']), 401);
  assert.equal(await rejected(endpoint(localPort, session), macProtocols, { headers: { origin: 'https://spoof.example' } }), 403);
  assert.equal(await rejected(endpoint(localPort, session), macProtocols, { headers: { 'x-forwarded-for': '198.51.100.1' } }), 403);
  const mac = await openRole(localPort, root, session, 'mac');
  assert.equal(mac._socket.remoteAddress, '127.0.0.1');
  assert.equal(mac._socket.remotePort, localPort);
  const other = randomBytes(32).toString('base64url');
  assert.equal(await rejected(endpoint(localPort, other), clientProtocols('mac', 'bridge',
    deriveCapability(root, other, 'mac', 'bridge'))), 429);
  const phone = await openRole(port, root, session, 'iphone', { headers: { 'x-forwarded-for': '198.51.100.2' } });
  const received = once(mac, 'message');
  phone.send('synthetic-local-lane');
  assert.equal((await received)[0].toString(), 'synthetic-local-lane');
});

test('failed startup and shutdown release both owned listeners', async () => {
  const root = randomBytes(32);
  const first = createRelay({ port: 0, admissionRoot: root, allowLoopbackMac: true });
  const { port } = await first.listen();
  const localPort = first.localMacAddress().port;
  const conflicting = createRelay({ port, admissionRoot: root, allowLoopbackMac: true });
  await assert.rejects(conflicting.listen(), { code: 'EADDRINUSE' });
  assert.equal(conflicting.address(), null);
  assert.equal(conflicting.localMacAddress(), null);
  await first.close();
  assert.equal(first.address(), null);
  assert.equal(first.localMacAddress(), null);
  for (const released of [port, localPort]) {
    const probe = net.createServer();
    await new Promise((resolve, reject) => {
      probe.once('error', reject); probe.listen(released, '127.0.0.1', resolve);
    });
    await new Promise(resolve => probe.close(resolve));
  }
});

test('unqueueable reset fails closed and transport diagnostics never copy close prose', async (t) => {
  const { reasonCode, log } = require('../relay/diagnostics');
  assert.equal(reasonCode(Buffer.from('PRIVATE path/token/body\n')), 'unspecified');
  assert.equal(reasonCode('unrecognized_lowercase_secret'), 'unspecified');
  assert.equal(reasonCode('peer_reset_timeout'), 'peer_reset_timeout');
  let line;
  log('fixed_event', { write: value => { line = value; } });
  assert.match(line, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z fixed_event\n$/);
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root, limits: { bufferedBytes: 1 } });
  t.after(() => relay.close());
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const macClosed = closeCode(mac);
  const phone = await openRole(port, root, session, 'iphone');
  const phoneClosed = closeCode(phone);
  assert.equal(await macClosed, 4008);
  assert.equal(await phoneClosed, 4005);
});

test('late send failure for replaced phone cannot terminate the retained Mac or successor', async (t) => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root });
  const original = WebSocket.prototype.send;
  let fail;
  WebSocket.prototype.send = function (data, options, callback) {
    if (this._isServer && data.toString() === 'late-send-error') {
      fail = callback;
      return original.call(this, data, options, () => {});
    }
    return original.call(this, data, options, callback);
  };
  t.after(async () => { WebSocket.prototype.send = original; await relay.close(); });
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac');
  const firstReset = once(mac, 'ping');
  const phone = await openRole(port, root, session, 'iphone');
  await firstReset;
  // A phone request/reply roundtrip puts the reset echo ahead of this response.
  const incoming = once(mac, 'message');
  phone.send('ready'); await incoming;
  const delivered = once(phone, 'message');
  mac.send('late-send-error'); await delivered;
  const oldClosed = closeCode(phone);
  const successor = await openRole(port, root, session, 'iphone');
  await oldClosed;
  fail(new Error('synthetic send failure'));
  const after = once(mac, 'message');
  successor.send('still-connected');
  assert.equal((await after)[0].toString(), 'still-connected');
  assert.equal(mac.readyState, WebSocket.OPEN);
  assert.equal(successor.readyState, WebSocket.OPEN);
});

test('unacknowledged reset accumulation is capped without admitting another routing epoch', async (t) => {
  const root = randomBytes(32);
  const relay = createRelay({ port: 0, admissionRoot: root });
  t.after(() => relay.close());
  const { port } = await relay.listen();
  const session = randomBytes(32).toString('base64url');
  const mac = await openRole(port, root, session, 'mac', { autoPong: false });
  const macClosed = once(mac, 'close');
  let phone = null;
  for (let i = 0; i < 8; i += 1) {
    const reset = once(mac, 'ping');
    const oldClosed = phone && closeCode(phone);
    phone = await openRole(port, root, session, 'iphone');
    await reset;
    if (oldClosed) assert.equal(await oldClosed, 4003);
    assert.equal(mac.readyState, WebSocket.OPEN);
  }
  const rejectedPhone = await openRole(port, root, session, 'iphone');
  const rejectedClosed = closeCode(rejectedPhone);
  const [code, reason] = await macClosed;
  assert.equal(code, 4008);
  assert.equal(reason.toString(), 'peer_reset_limit');
  assert.equal(await rejectedClosed, 4005);
});
