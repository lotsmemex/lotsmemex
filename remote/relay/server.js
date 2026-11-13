// Blind room-routing pattern adapted from Remodex relay/relay.js and server.js.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Modified for MemeX: role-scoped Upgrade admission, bounded one-phone rooms,
// no accounts/persistence/push, and no payload/session/path logging.
'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const { WebSocket, WebSocketServer } = require('ws');
const { log, closed } = require('./diagnostics');
const {
  FIXED_SUBPROTOCOL,
  admissionRoot: parseAdmissionRoot,
  authorizeProtocols,
} = require('../admission');

const HEARTBEAT_MS = 30_000;
const MAX_WIRE_BYTES = 384 * 1024;
const PEER_RESET = Buffer.from('memex_peer_reset_v1', 'ascii');
const RESET_TIMEOUT_MS = 3_000;
const MAX_PENDING_RESETS = 8;
const SESSION_PATTERN = /^[A-Za-z0-9_-]{43}$/;
const STATIC_ROOT = path.join(__dirname, '../web');

function diagnosticHtml(file) {
  return Buffer.from(
    fs.readFileSync(path.join(STATIC_ROOT, file), 'utf8'),
    'utf8',
  );
}

const DIAGNOSTIC_HTML = Object.freeze({
  library: diagnosticHtml('index.html'),
  settings: diagnosticHtml('settings.html'),
  account: diagnosticHtml('account.html'),
});
const DIAGNOSTIC_INLINE_HASHES = Object.fromEntries(Object.entries(DIAGNOSTIC_HTML)
  .map(([name, body]) => {
    const inline = body.toString('utf8').match(/<script>([\s\S]*?)<\/script>/)?.[1];
    return [name, inline ? crypto.createHash('sha256').update(inline).digest('base64') : null];
  }));
const STATIC_FILES = new Map([
  ['/', {
    body: DIAGNOSTIC_HTML.library,
    type: 'text/html; charset=utf-8',
    headerOptions: { inlineHash: DIAGNOSTIC_INLINE_HASHES.library, connect: true },
  }],
  ['/settings', {
    body: DIAGNOSTIC_HTML.settings,
    type: 'text/html; charset=utf-8',
    headerOptions: { inlineHash: DIAGNOSTIC_INLINE_HASHES.settings, camera: true, connect: true },
  }],
  ['/account', {
    body: DIAGNOSTIC_HTML.account,
    type: 'text/html; charset=utf-8',
    headerOptions: { inlineHash: null, connect: false },
  }],
  ['/remote/v1/', {
    body: DIAGNOSTIC_HTML.library,
    type: 'text/html; charset=utf-8',
    headerOptions: { inlineHash: DIAGNOSTIC_INLINE_HASHES.library, connect: true },
  }],
  ['/remote/v1/app.mjs', { file: 'app.mjs', type: 'text/javascript; charset=utf-8' }],
  ['/remote/v1/protocol.mjs', { file: 'protocol.mjs', type: 'text/javascript; charset=utf-8' }],
  ['/remote/v1/storage.mjs', { file: 'storage.mjs', type: 'text/javascript; charset=utf-8' }],
  ['/remote/v1/scanner.mjs', { file: 'scanner.mjs', type: 'text/javascript; charset=utf-8' }],
  ['/remote/v1/app.css', { file: 'app.css', type: 'text/css; charset=utf-8' }],
  ['/remote/v1/config.mjs', { file: 'config.mjs', type: 'text/javascript; charset=utf-8' }],
].map(([route, entry]) => [route, entry.body ? entry : {
  body: fs.readFileSync(path.join(STATIC_ROOT, entry.file)),
  type: entry.type,
  headerOptions: { connect: true },
}]));
STATIC_FILES.set('/remote/v1/jsqr.js', {
  body: fs.readFileSync(require.resolve('jsqr')),
  type: 'text/javascript; charset=utf-8',
});
if (!DIAGNOSTIC_INLINE_HASHES.library || !DIAGNOSTIC_INLINE_HASHES.settings) {
  throw new Error('invalid_static_client');
}
const DEFAULT_LIMITS = Object.freeze({
  totalConnections: 64,
  connectionsPerSource: 8,
  rooms: 32,
  framesPerSecond: 30,
  bytesPerSecond: 768 * 1024,
  bufferedBytes: 512 * 1024,
  idleRoomMs: 2 * 60_000,
});

function validateLimits(overrides = {}) {
  const limits = { ...DEFAULT_LIMITS, ...overrides };
  for (const [name, value] of Object.entries(limits)) {
    if (!Number.isSafeInteger(value) || value < 1) throw new Error(`invalid_limit_${name}`);
  }
  return limits;
}

function rejectUpgrade(socket, status) {
  // Status line only; no path, origin, protocol header or source address.
  log(`memex_relay_upgrade_rejected status=${status.split(' ')[0]}`);
  if (!socket.destroyed) {
    socket.end(
      `HTTP/1.1 ${status}\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Length: 0\r\n\r\n`,
    );
  }
}

function sourceFor(request, trustProxy) {
  if (!trustProxy) return request.socket.remoteAddress || 'unknown';
  const forwarded = request.headers['x-forwarded-for'];
  if (typeof forwarded !== 'string' || forwarded.length > 512) return null;
  const source = forwarded.split(',')[0].trim();
  return source.length > 0 && source.length <= 128 && !/[\s\r\n]/.test(source) ? source : null;
}

function overRate(socket, byteLength, now, limits) {
  if (now - socket.rateWindowStartedAt >= 1_000) {
    socket.rateWindowStartedAt = now;
    socket.rateFrames = 0;
    socket.rateBytes = 0;
  }
  socket.rateFrames += 1;
  socket.rateBytes += byteLength;
  return socket.rateFrames > limits.framesPerSecond || socket.rateBytes > limits.bytesPerSecond;
}

function canForward(peer, byteLength, bufferedBytes) {
  return peer.bufferedAmount + byteLength <= bufferedBytes;
}

function exactBrowserOrigin(value) {
  if (value === undefined) return null;
  let url;
  try { url = new URL(value); } catch { throw new Error('invalid_browser_origin'); }
  if (
    url.protocol !== 'https:'
    || url.username || url.password || url.port || url.pathname !== '/'
    || url.search || url.hash || url.origin !== value
  ) throw new Error('invalid_browser_origin');
  return url.origin;
}

function diagnosticClient(publicUrl) {
  let url;
  try { url = new URL(publicUrl); } catch { throw new Error('invalid_diagnostic_public_url'); }
  const local = url.protocol === 'http:'
    && ['127.0.0.1', 'localhost', '::1'].includes(url.hostname);
  if (
    (url.protocol !== 'https:' && !local) || url.username || url.password || (!local && url.port)
    || url.pathname !== '/' || url.search || url.hash || url.toString() !== publicUrl
  ) throw new Error('invalid_diagnostic_public_url');
  const relay = new URL(publicUrl);
  relay.protocol = local ? 'ws:' : 'wss:';
  relay.pathname = '/relay';
  return Object.freeze({
    browserOrigin: url.origin,
    webClientUrl: url.toString(),
    relayUrl: relay.toString(),
    allowLocal: local,
  });
}

function staticHeadersFor(relayUrl, {
  inlineHash = DIAGNOSTIC_INLINE_HASHES.library,
  camera = false,
  connect = true,
  modules = false,
  images = false,
  sameOriginFetch = false,
  fonts = false,
} = {}) {
  const relay = new URL(relayUrl);
  const connectOrigin = relay.origin;
  const scripts = inlineHash ? `script-src 'self' 'sha256-${inlineHash}'`
    : (modules ? "script-src 'self'" : "script-src 'none'");
  return Object.freeze({
    'cache-control': 'no-store, no-transform',
    'content-security-policy': `default-src 'none'; ${scripts}; style-src 'self'; connect-src ${sameOriginFetch ? "'self' " : ''}${connect ? connectOrigin : (sameOriginFetch ? '' : "'none'")}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'; worker-src 'none'; manifest-src 'none'; img-src ${images ? "'self' blob:" : "'none'"}; font-src ${fonts ? "'self'" : "'none'"}; media-src ${camera ? "'self' blob:" : "'none'"}`,
    'cross-origin-opener-policy': 'same-origin',
    'cross-origin-resource-policy': 'same-origin',
    'permissions-policy': `camera=${camera ? '(self)' : '()'}, microphone=(), geolocation=(), payment=(), usb=(), bluetooth=()`,
    'referrer-policy': 'no-referrer',
    'x-amp-review-widget': 'off',
    'x-content-type-options': 'nosniff',
    'x-frame-options': 'DENY',
  });
}

function serveStatic(request, response, client, applicationRoutes = new Map()) {
  if (!client) return false;
  if (!['GET', 'HEAD'].includes(request.method)) return false;
  let url;
  try {
    url = new URL(request.url, 'http://relay.invalid');
  } catch {
    return false;
  }
  if (url.search || url.hash) return false;
  const entry = applicationRoutes.get(url.pathname) || STATIC_FILES.get(url.pathname);
  if (!entry) return false;
  const body = url.pathname === '/remote/v1/config.mjs'
    ? Buffer.from(`export const WEB_CLIENT_URL = ${JSON.stringify(client.webClientUrl)};\nexport const RELAY_URL = ${JSON.stringify(client.relayUrl)};\nexport const ALLOW_LOCAL = ${client.allowLocal};\n`, 'utf8')
    : entry.body;
  for (const [name, value] of Object.entries(
    staticHeadersFor(client.relayUrl, entry.headerOptions),
  )) response.setHeader(name, value);
  response.setHeader('content-type', entry.type);
  response.setHeader('content-length', body.length);
  response.writeHead(200);
  response.end(request.method === 'HEAD' ? undefined : body);
  return true;
}

function createRelay({
  host = '127.0.0.1',
  port = 8787,
  heartbeatMs = HEARTBEAT_MS,
  admissionRoot,
  browserOrigin,
  diagnosticPublicUrl,
  diagnosticSameOrigin = false,
  application = null,
  limits: limitOverrides,
  trustProxy = false,
  allowLoopbackMac = false,
  now = () => Date.now(),
} = {}) {
  const root = parseAdmissionRoot(admissionRoot);
  if (allowLoopbackMac && host !== '127.0.0.1') throw new Error('invalid_loopback_config');
  if ((diagnosticPublicUrl !== undefined) !== diagnosticSameOrigin) {
    throw new Error('invalid_diagnostic_config');
  }
  let client = diagnosticSameOrigin ? diagnosticClient(diagnosticPublicUrl) : null;
  // The mobile app mounts a reviewed static manifest, not a directory server. The
  // relay itself always binds loopback; a public HTTPS origin reaches it only
  // through the owner's named tunnel.
  if (application && (!client || host !== '127.0.0.1'
    || !/^\/[a-z0-9/-]+\.html$/.test(application.path)
    || !(application.routes instanceof Map))) throw new Error('invalid_diagnostic_config');
  if (application) client = { ...client, webClientUrl: new URL(application.path, client.webClientUrl).href };
  const configuredBrowserOrigin = exactBrowserOrigin(browserOrigin);
  if (client && configuredBrowserOrigin && client.browserOrigin !== configuredBrowserOrigin) {
    throw new Error('invalid_diagnostic_config');
  }
  let allowedBrowserOrigin = configuredBrowserOrigin || client?.browserOrigin || null;
  const limits = validateLimits(limitOverrides);
  const rooms = new Map();
  const sourceConnections = new Map();
  let totalConnections = 0;
  const server = http.createServer((request, response) => {
    if (serveStatic(request, response, client, application?.routes)) return;
    response.setHeader('cache-control', 'no-store');
    response.setHeader('x-content-type-options', 'nosniff');
    if (request.method === 'GET' && request.url === '/healthz') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ ok: true }));
      return;
    }
    response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
    response.end('not_found');
  });
  // Separate ingress, never exposed by the public tunnel. Address/headers on the
  // public listener cannot select this lane. Admission and budgets remain shared.
  const localServer = allowLoopbackMac ? http.createServer((_request, response) => {
    response.writeHead(404); response.end();
  }) : null;
  const wss = new WebSocketServer({
    noServer: true,
    maxPayload: MAX_WIRE_BYTES,
    perMessageDeflate: false,
    handleProtocols(protocols) {
      return protocols.has(FIXED_SUBPROTOCOL) ? FIXED_SUBPROTOCOL : false;
    },
  });

  function roomFor(sessionId) {
    let room = rooms.get(sessionId);
    if (!room) {
      room = { mac: null, iphone: null, phoneEpoch: 0, lastActivityAt: now() };
      rooms.set(sessionId, room);
    }
    return room;
  }

  function closeSocket(socket, code, reason) {
    if (socket?.readyState === WebSocket.OPEN || socket?.readyState === WebSocket.CONNECTING) {
      socket.close(code, reason);
    }
  }

  function resetPhone(room) {
    const mac = room.mac;
    const phone = room.iphone;
    if (mac?.readyState !== WebSocket.OPEN) return false;
    if (mac.resetPending >= MAX_PENDING_RESETS) {
      closeSocket(mac, 4008, 'peer_reset_limit');
      closeSocket(phone, 4005, 'peer_reset_limit');
      return false;
    }
    const epoch = ++room.phoneEpoch;
    const failed = () => {
      if (room.mac !== mac || room.phoneEpoch !== epoch) return;
      closeSocket(mac, 4008, 'backpressure');
      closeSocket(phone, 4005, 'peer_backpressure');
    };
    if (!canForward(mac, PEER_RESET.length + 16, limits.bufferedBytes)) {
      failed(); return false;
    }
    try {
      if (mac.resetPending === 0) {
        mac.resetDeadline = now() + RESET_TIMEOUT_MS;
        mac.resetTimer = setTimeout(() => {
          if (room.mac === mac && mac.resetPending > 0) closeSocket(mac, 4005, 'peer_reset_timeout');
        }, RESET_TIMEOUT_MS);
        mac.resetTimer.unref();
      }
      mac.resetPending += 1;
      mac.ping(PEER_RESET, false, error => { if (error) failed(); });
      return true;
    } catch { failed(); return false; }
  }

  function remove(sessionId, role, socket) {
    clearTimeout(socket.resetTimer);
    if (socket.counted) {
      socket.counted = false;
      totalConnections -= 1;
      const count = (sourceConnections.get(socket.source) || 1) - 1;
      if (count === 0) sourceConnections.delete(socket.source);
      else sourceConnections.set(socket.source, count);
    }
    const room = rooms.get(sessionId);
    if (!room || room[role] !== socket) return;
    room[role] = null;
    if (role === 'mac') {
      closeSocket(room.iphone, 4002, 'peer_offline');
      room.iphone = null;
    } else resetPhone(room);
    if (!room.mac && !room.iphone) rooms.delete(sessionId);
  }

  wss.on('connection', (socket, { sessionId, role, source }) => {
    socket.counted = true;
    socket.source = source;
    totalConnections += 1;
    sourceConnections.set(source, (sourceConnections.get(source) || 0) + 1);
    socket.rateWindowStartedAt = now();
    socket.rateFrames = 0;
    socket.rateBytes = 0;
    socket.resetPending = 0;
    const room = role === 'iphone' ? rooms.get(sessionId) : roomFor(sessionId);
    if (role === 'iphone' && room?.mac?.readyState !== WebSocket.OPEN) {
      socket.close(4002, 'mac_offline');
    } else {
      const previous = room[role];
      room[role] = socket;
      room.lastActivityAt = now();
      if (role === 'iphone') resetPhone(room);
      else if (room.iphone) {
        closeSocket(room.iphone, 4005, 'peer_replaced');
        room.iphone = null;
      }
      closeSocket(previous, 4003, 'replaced');
    }
    socket.isAlive = true;
    socket.on('pong', (data) => {
      socket.isAlive = true;
      if (room?.[role] === socket && role === 'mac' && data.equals(PEER_RESET) && socket.resetPending > 0) {
        socket.resetPending -= 1;
        if (socket.resetPending === 0) clearTimeout(socket.resetTimer);
      }
      // A live heartbeat is activity: idle expiry is for abandoned rooms, not for a
      // paired phone and Mac that are simply quiet while the user reads.
      if (room && room[role] === socket) room.lastActivityAt = now();
    });
    socket.on('message', (data, isBinary) => {
      if (
        isBinary
        || data.length > MAX_WIRE_BYTES
        || !room
        || room[role] !== socket
        || overRate(socket, data.length, now(), limits)
      ) {
        socket.close(isBinary || data.length > MAX_WIRE_BYTES ? 4000 : 4008, 'limit_exceeded');
        return;
      }
      room.lastActivityAt = now();
      const peerRole = role === 'mac' ? 'iphone' : 'mac';
      const peer = room[peerRole];
      // Discard old-channel responses still in flight before reset was received.
      // New handshake responses follow the reset Pong on the same ordered socket.
      if (role === 'mac' && socket.resetPending > 0) return;
      if (peer?.readyState === WebSocket.OPEN) {
        if (!canForward(peer, data.length, limits.bufferedBytes)) {
          closeSocket(peer, 4008, 'backpressure');
          socket.close(4005, 'peer_backpressure');
          return;
        }
        peer.send(data, { binary: false }, (error) => {
          if (error && room[role] === socket && room[peerRole] === peer) {
            peer.terminate();
            if (role === 'iphone') closeSocket(socket, 4005, 'peer_offline');
          }
        });
      } else if (role === 'iphone') {
        socket.close(4004, 'message_not_delivered');
      }
    });
    socket.on('close', (code, reason) => {
      closed(role, code, socket.closeCause || reason);
      remove(sessionId, role, socket);
    });
    socket.on('error', () => socket.terminate());
  });

  function upgrade(request, socket, head, localMac = false) {
    let url;
    try {
      url = new URL(request.url, 'http://relay.invalid');
    } catch {
      rejectUpgrade(socket, '400 Bad Request');
      return;
    }
    const match = url.pathname.match(/^\/relay\/([^/]+)$/);
    const sessionId = match?.[1];
    if (
      request.method !== 'GET'
      || !sessionId
      || !SESSION_PATTERN.test(sessionId)
      || url.search !== ''
      || url.hash !== ''
    ) {
      rejectUpgrade(socket, '400 Bad Request');
      return;
    }
    const authorization = authorizeProtocols(
      request.headers['sec-websocket-protocol'],
      root,
      sessionId,
    );
    if (!authorization) {
      rejectUpgrade(socket, '401 Unauthorized');
      return;
    }
    if (localMac && (authorization.role !== 'mac' || authorization.clientMode !== 'bridge'
      || request.headers['x-forwarded-for'] !== undefined)) {
      rejectUpgrade(socket, '403 Forbidden');
      return;
    }
    const origin = request.headers.origin;
    if (
      (authorization.clientMode === 'browser' && (
        !allowedBrowserOrigin || origin !== allowedBrowserOrigin
      ))
      || (authorization.clientMode !== 'browser' && origin !== undefined)
    ) {
      rejectUpgrade(socket, '403 Forbidden');
      return;
    }
    const source = localMac ? 'loopback-mac' : sourceFor(request, trustProxy);
    if (!source) {
      rejectUpgrade(socket, '400 Bad Request');
      return;
    }
    if (
      totalConnections >= limits.totalConnections
      || (sourceConnections.get(source) || 0) >= limits.connectionsPerSource
      || (!rooms.has(sessionId) && rooms.size >= limits.rooms)
    ) {
      rejectUpgrade(socket, '429 Too Many Requests');
      return;
    }
    wss.handleUpgrade(request, socket, head, (webSocket) => {
      wss.emit('connection', webSocket, { sessionId, role: authorization.role, source });
    });
  }
  server.on('upgrade', (request, socket, head) => upgrade(request, socket, head));
  localServer?.on('upgrade', (request, socket, head) => upgrade(request, socket, head, true));

  function sweep() {
    const current = now();
    for (const [sessionId, room] of rooms) {
      if (room.mac?.resetPending > 0 && current >= room.mac.resetDeadline) {
        closeSocket(room.mac, 4005, 'peer_reset_timeout');
      }
      if (current - room.lastActivityAt >= limits.idleRoomMs) {
        closeSocket(room.mac, 4009, 'idle_timeout');
        closeSocket(room.iphone, 4009, 'idle_timeout');
        if (!room.mac && !room.iphone) rooms.delete(sessionId);
      }
    }
  }

  const heartbeat = setInterval(() => {
    sweep();
    for (const socket of wss.clients) {
      if (socket.readyState !== WebSocket.OPEN) continue;
      if (!socket.isAlive) {
        socket.closeCause = 'heartbeat_timeout';
        socket.terminate();
      } else {
        socket.isAlive = false;
        socket.ping();
      }
    }
  }, heartbeatMs);
  heartbeat.unref();

  function close() {
    clearInterval(heartbeat);
    const closing = [server, localServer].filter(Boolean).map(s => new Promise((resolve, reject) => {
      if (!s.listening) { resolve(); return; }
      s.close((error) => (error ? reject(error) : resolve()));
      s.closeAllConnections();
    }));
    for (const socket of wss.clients) socket.terminate();
    wss.close();
    return Promise.all(closing);
  }

  function listenOn(s, listenPort, listenHost) {
    return new Promise((resolve, reject) => {
      s.once('error', reject);
      s.listen(listenPort, listenHost, () => { s.off('error', reject); resolve(); });
    });
  }

  return {
    address: () => server.address(),
    localMacAddress: () => localServer?.address() || null,
    close,
    listen: async () => {
      try {
        await listenOn(server, port, host);
        if (localServer) await listenOn(localServer, 0, '127.0.0.1');
        if (client?.allowLocal && new URL(client.webClientUrl).port === '0') {
          const publicUrl = new URL(diagnosticPublicUrl);
          publicUrl.port = String(server.address().port);
          client = diagnosticClient(publicUrl.href);
          if (application) client = { ...client, webClientUrl: new URL(application.path, publicUrl).href };
          allowedBrowserOrigin = client.browserOrigin;
        }
        return server.address();
      } catch (error) {
        await close();
        throw error;
      }
    },
    sweep,
  };
}

if (require.main === module) {
  const port = Number.parseInt(process.env.PORT || '8787', 10);
  try {
    if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('invalid_port');
    const relay = createRelay({
      host: process.env.HOST || '127.0.0.1',
      port,
      admissionRoot: process.env.MEMEX_RELAY_ADMISSION_ROOT,
      browserOrigin: process.env.MEMEX_BROWSER_ORIGIN,
      diagnosticPublicUrl: process.env.MEMEX_DIAGNOSTIC_PUBLIC_URL,
      diagnosticSameOrigin: process.env.MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN === '1',
      trustProxy: process.env.MEMEX_RELAY_TRUST_PROXY === '1',
    });
    relay.listen().then((address) => {
      log(`memex_relay_ready port=${address.port}`);
    }).catch(() => {
      log('relay_start_failed', process.stderr);
      process.exitCode = 1;
    });
  } catch (error) {
    const code = [
      'invalid_port', 'invalid_admission_root', 'invalid_browser_origin',
      'invalid_diagnostic_public_url', 'invalid_diagnostic_config',
    ].includes(error.message)
      ? error.message
      : 'relay_start_failed';
    log(code, process.stderr);
    process.exitCode = 1;
  }
}

module.exports = {
  DEFAULT_LIMITS,
  HEARTBEAT_MS,
  MAX_WIRE_BYTES,
  createRelay,
  exactBrowserOrigin,
  serveStatic,
  staticHeadersFor,
};
