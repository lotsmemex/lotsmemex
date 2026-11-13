// Reconnect lifecycle adapted from the focused Remodex bridge.js transport slice.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Modified for the bounded MemeX owner-daemon RPC bridge.
'use strict';

const { dataDir } = require('../data-dir');
const path = require('node:path');
const { WebSocket } = require('ws');
const { log, closed, reasonCode } = require('../relay/diagnostics');
const { admissionRoot, clientProtocols, deriveCapability } = require('../admission');
const { base64url } = require('../protocol');
const {
  BridgeControlServer,
  PairingController,
} = require('./bridge-control');
const { createRpcRouter } = require('./rpc-router');
const {
  clearPairingImage,
  presentPairingCode,
  stagePairingCode,
} = require('./pair-presenter');
const { MacSecureSession } = require('./secure-session');
const {
  MacKeychainStateStore,
  acquireProcessLease,
  beginFreshPairing,
  pendingPairingState,
} = require('./state-store');

const BROWSER_PAIRING_PREFIX = 'MEMEX PAIR 1:';

function parseRelayUrl(value) {
  let url;
  try { url = new URL(value); } catch { throw new Error('invalid_relay_url'); }
  const localInsecure = url.protocol === 'ws:' && ['127.0.0.1', 'localhost', '::1'].includes(url.hostname);
  if (
    (url.protocol !== 'wss:' && !localInsecure)
    || url.username || url.password || url.search || url.hash
    || url.pathname !== '/relay' || url.toString() !== value
  ) throw new Error('invalid_relay_url');
  url.pathname = '/relay';
  return url;
}

function parseWebClientUrl(value, { allowLocal = false } = {}) {
  let url;
  try { url = new URL(value); } catch { throw new Error('invalid_web_client_url'); }
  const localInsecure = allowLocal && url.protocol === 'http:'
    && ['127.0.0.1', 'localhost', '::1'].includes(url.hostname);
  if (
    (url.protocol !== 'https:' && !localInsecure)
    || url.username || url.password || (!localInsecure && url.port) || url.search || url.hash
    || !url.pathname.startsWith('/') || url.pathname.includes('//')
    || url.pathname.endsWith('/') || url.toString() !== value
  ) throw new Error('invalid_web_client_url');
  return url;
}

function relayEndpoint(base, sessionId) {
  const url = new URL(base);
  url.pathname = `/relay/${sessionId}`;
  return url.toString();
}

function diagnosticWebClientUrl(relayUrl) {
  const relay = parseRelayUrl(relayUrl);
  relay.protocol = relay.protocol === 'wss:' ? 'https:' : 'http:';
  relay.pathname = '/';
  relay.search = '';
  relay.hash = '';
  return relay;
}

function pairingPayload(state, relayUrl, root, clientMode = 'browser', webClientValue, options = {}) {
  if (!state.bootstrap_secret || state.pairing_expires_at_ms === 0) {
    throw new Error('pairing_not_active');
  }
  if (!['browser', 'native'].includes(clientMode)) throw new Error('invalid_pairing_client');
  const payload = {
    v: 1,
    relay: parseRelayUrl(relayUrl).toString(),
    session_id: state.session_id,
    pairing_generation: state.generation,
    mac_device_id: state.mac_device_id,
    mac_identity_public_key: state.mac_identity_public_key,
    bootstrap_secret: state.bootstrap_secret,
    iphone_admission_capability: deriveCapability(root, state.session_id, 'iphone', clientMode),
    expires_at_ms: state.pairing_expires_at_ms,
  };
  if (clientMode === 'browser') {
    payload.web_client_url = parseWebClientUrl(webClientValue, options).toString();
  }
  return payload;
}

function browserPairingCode(state, relayUrl, root, webClientValue, options = {}) {
  const client = parseWebClientUrl(webClientValue, options);
  const encoded = base64url(Buffer.from(JSON.stringify(
    pairingPayload(state, relayUrl, root, 'browser', client.toString(), options),
  ), 'utf8'));
  return `${BROWSER_PAIRING_PREFIX}${encoded}`;
}

function nativePairingCode(state, relayUrl, root) {
  return `MEMEX1:${base64url(Buffer.from(JSON.stringify(
    pairingPayload(state, relayUrl, root, 'native'),
  ), 'utf8'))}`;
}

class BridgeRunner {
  constructor({
    relayUrl,
    socketPath,
    admissionRoot: root,
    stateStore = new MacKeychainStateStore(),
    WebSocketClass = WebSocket,
    onPairingConsumed = () => {},
    connectionMode = 'stable',
    router = null,
    authorization = null,
  }) {
    this.relayUrl = parseRelayUrl(relayUrl);
    this.socketPath = socketPath;
    this.admissionRoot = admissionRoot(root);
    this.stateStore = stateStore;
    this.WebSocketClass = WebSocketClass;
    this.onPairingConsumed = onPairingConsumed;
    this.router = router;
    this.authorization = authorization;
    if (!['stable', 'pending'].includes(connectionMode)) throw new Error('invalid_connection_mode');
    this.connectionMode = connectionMode;
    this.stopped = true;
    this.attempt = 0;
    this.socket = null;
    this.reconnectTimer = null;
    this.heartbeatTimer = null;
    this.dispatchBudget = { active: 0 };
  }

  start(state = null) {
    this.stopped = false;
    this.connect(state);
  }

  stop() {
    this.stopped = true;
    clearTimeout(this.reconnectTimer);
    clearInterval(this.heartbeatTimer);
    this.socket?.close(1000, 'stopped');
  }

  reconnectForStateChange(state = null) {
    if (this.stopped) return;
    clearTimeout(this.reconnectTimer);
    clearInterval(this.heartbeatTimer);
    this.reconnectTimer = null;
    this.heartbeatTimer = null;
    this.attempt = 0;
    const oldSocket = this.socket;
    this.socket = null;
    try { oldSocket?.terminate(); } catch {}
    this.connect(state);
  }

  promotePending() {
    if (this.connectionMode !== 'pending') throw new Error('invalid_connection_mode');
    this.connectionMode = 'stable';
  }

  connectionState(state = null) {
    const current = state || this.stateStore.loadOrCreate();
    if (this.connectionMode === 'stable') return current;
    if (current.bootstrap_secret && current.pairing_expires_at_ms) return current;
    return pendingPairingState(current);
  }

  connect(state = null) {
    if (this.stopped) return;
    const current = this.connectionState(state);
    const capability = deriveCapability(this.admissionRoot, current.session_id, 'mac', 'bridge');
    const socket = new this.WebSocketClass(
      relayEndpoint(this.relayUrl, current.session_id),
      clientProtocols('mac', 'bridge', capability),
      { maxPayload: 384 * 1024, perMessageDeflate: false },
    );
    this.socket = socket;
    let alive = true;
    let receiveChain = Promise.resolve();
    const router = this.router || createRpcRouter({ socketPath: this.socketPath });
    const session = new MacSecureSession({
      stateStore: this.stateStore,
      router,
      authorization: this.authorization,
      dispatchBudget: this.dispatchBudget,
      // The scoped library router answers by request id, so its calls may overlap.
      concurrent: this.router !== null,
      onAsyncError: () => { if (this.socket === socket) socket.close(4000, 'secure_transport_error'); },
      send: (message) => {
        if (this.socket !== socket || socket.readyState !== WebSocket.OPEN) {
          throw new Error('relay_offline');
        }
        socket.send(JSON.stringify(message));
      },
      onPairingConsumed: this.onPairingConsumed,
    });
    socket.on('open', () => {
      if (this.socket !== socket) {
        socket.terminate();
        return;
      }
      this.attempt = 0;
      log('memex_mac_bridge_connected');
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = setInterval(() => {
        if (!alive) {
          socket.closeCause = 'heartbeat_timeout';
          socket.terminate();
          return;
        }
        alive = false;
        socket.ping();
      }, 25_000);
      this.heartbeatTimer.unref();
    });
    socket.on('pong', () => { if (this.socket === socket) alive = true; });
    socket.on('ping', (data) => {
      if (this.socket === socket && data.equals(Buffer.from('memex_peer_reset_v1', 'ascii'))) {
        session.invalidate();
      }
    });
    socket.on('message', (data, isBinary) => {
      if (this.socket !== socket) return;
      if (isBinary) {
        socket.close(4000, 'invalid_frame');
        return;
      }
      const epoch = session.epoch;
      receiveChain = receiveChain
        .then(() => {
          if (this.socket === socket && session.epoch === epoch) return session.receive(data);
        })
        .catch((error) => {
          if (this.socket !== socket || session.epoch !== epoch) return;
          // Reason code only (fixed vocabulary); never session ids, payloads or library data.
          log(`memex_mac_bridge_phone_rejected reason=${reasonCode(error?.message)}`);
          socket.close(4000, 'secure_transport_error');
        });
    });
    socket.on('error', () => {});
    socket.on('close', (code, reason) => {
      session.invalidate();
      closed('mac', code, socket.closeCause || reason);
      if (this.socket === socket) {
        clearInterval(this.heartbeatTimer);
        this.heartbeatTimer = null;
      }
      if (!this.stopped && this.socket === socket) {
        this.socket = null;
        log('memex_mac_bridge_disconnected');
        this.scheduleReconnect();
      }
    });
  }

  scheduleReconnect() {
    this.attempt += 1;
    const base = Math.min(1_000 * this.attempt, 5_000);
    const jitter = Math.floor(Math.random() * Math.min(base, 2_000));
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = setTimeout(() => this.connect(), base + jitter);
  }
}

function promoteTrustedRunner({ pairingController, runner, pendingRunner, activeRunner }) {
  if (pendingRunner !== runner) return { activeRunner, pendingRunner };
  pairingController.pairingConsumed();
  runner.promotePending();
  try { activeRunner.stop(); } catch {}
  return { activeRunner: runner, pendingRunner: null };
}

async function main() {
  const action = process.argv[2];
  if (!['pair', 'pair-native', 'run'].includes(action)) throw new Error('usage_error');
  const relayUrl = process.env.MEMEX_RELAY_URL;
  if (!relayUrl) throw new Error('missing_relay_url');
  let webClientValue = process.env.MEMEX_WEB_CLIENT_URL;
  const diagnosticSameOrigin = process.env.MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN === '1';
  if (action === 'pair' && !webClientValue && diagnosticSameOrigin) {
    webClientValue = diagnosticWebClientUrl(relayUrl).toString();
  }
  if (action !== 'pair-native' && !webClientValue) throw new Error('missing_web_client_url');
  const root = admissionRoot(process.env.MEMEX_RELAY_ADMISSION_ROOT);
  const socketPath = process.env.MEMEX_AGENT_SOCKET
    || path.join(dataDir(), 'agent.sock');
  const store = new MacKeychainStateStore();
  const lease = acquireProcessLease();
  let removePairingImage = () => {};
  let pairingExpiryTimer = null;
  let activeRunner = null;
  let pendingRunner = null;
  let pairingController = null;
  let controlServer = null;
  let initialPairingState = null;
  try {
    if (action === 'pair' || action === 'pair-native') {
      const state = beginFreshPairing(store);
      initialPairingState = state;
      const qrValue = action === 'pair'
        ? browserPairingCode(
          state, relayUrl, root, webClientValue, { allowLocal: diagnosticSameOrigin },
        )
        : nativePairingCode(state, relayUrl, root);
      const cleanup = await presentPairingCode(qrValue);
      removePairingImage = () => {
        clearTimeout(pairingExpiryTimer);
        cleanup();
      };
      pairingExpiryTimer = setTimeout(
        removePairingImage,
        Math.max(0, state.pairing_expires_at_ms - Date.now()),
      );
      pairingExpiryTimer.unref();
      process.once('exit', removePairingImage);
      log('pairing_window_ready');
    }
    activeRunner = new BridgeRunner({
      relayUrl,
      socketPath,
      admissionRoot: root,
      stateStore: store,
      connectionMode: action === 'run' ? 'stable' : 'pending',
      onPairingConsumed: () => {
        if (pairingController) pairingController.pairingConsumed();
        else removePairingImage();
      },
    });
    if (action === 'run') {
      clearPairingImage();
      pairingController = new PairingController({
        stateStore: store,
        createPairingCode: (state) => browserPairingCode(
          state,
          relayUrl,
          root,
          webClientValue,
        ),
        presentPairingCode: stagePairingCode,
        startPendingBridge: (state) => {
          pendingRunner?.stop();
          const runner = new BridgeRunner({
            relayUrl,
            socketPath,
            admissionRoot: root,
            stateStore: store,
            connectionMode: 'pending',
            onPairingConsumed: () => {
              ({ activeRunner, pendingRunner } = promoteTrustedRunner({
                pairingController, runner, pendingRunner, activeRunner,
              }));
            },
          });
          pendingRunner = runner;
          runner.start(state);
        },
        stopPendingBridge: () => {
          pendingRunner?.stop();
          pendingRunner = null;
        },
        replaceActiveBridge: (state) => activeRunner.reconnectForStateChange(state),
      });
      pairingController.initialize();
      controlServer = new BridgeControlServer({ controller: pairingController });
      await controlServer.start();
    }
    let shuttingDown = false;
    const shutdown = async () => {
      if (shuttingDown) return;
      shuttingDown = true;
      pairingController?.shutdown();
      pendingRunner?.stop();
      activeRunner.stop();
      await controlServer?.close();
      lease.release();
    };
    process.once('SIGINT', () => { shutdown().finally(() => process.exit(0)); });
    process.once('SIGTERM', () => { shutdown().finally(() => process.exit(0)); });
    activeRunner.start(initialPairingState);
  } catch (error) {
    pairingController?.shutdown();
    pendingRunner?.stop();
    activeRunner?.stop();
    await controlServer?.close();
    removePairingImage();
    lease.release();
    throw error;
  }
}

if (require.main === module) {
  main().catch((error) => {
    const allowed = new Set([
      'missing_relay_url', 'invalid_relay_url', 'missing_web_client_url',
      'invalid_web_client_url', 'keychain_read_failed',
      'keychain_write_failed', 'invalid_admission_root', 'state_lock_busy',
      'unsafe_state_lock', 'bridge_already_running', 'bridge_lock_failed', 'usage_error',
      'unsafe_pairing_directory', 'unsafe_pairing_image', 'pairing_window_unavailable',
      'unsafe_control_socket',
    ]);
    log(allowed.has(error.message) ? error.message : 'bridge_start_failed', process.stderr);
    process.exitCode = 1;
  });
}

module.exports = {
  BROWSER_PAIRING_PREFIX,
  BridgeRunner,
  browserPairingCode,
  nativePairingCode,
  pairingPayload,
  parseRelayUrl,
  parseWebClientUrl,
  promoteTrustedRunner,
  relayEndpoint,
  diagnosticWebClientUrl,
};
