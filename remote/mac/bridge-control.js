'use strict';

const { dataDir } = require('../data-dir');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const {
  beginFreshPairing,
  cancelPendingPairing,
  expirePairing,
  revokeTrust,
} = require('./state-store');

const DEFAULT_CONTROL_SOCKET_PATH = path.join(dataDir(), 'remote-managed/bridge-control.sock');
const MAXIMUM_CONTROL_FRAME = 1_024;
const CONTROL_TIMEOUT_MS = 3_000;
const CONTROL_ACTION_TIMEOUT_MS = 75_000;
const PRESENTATION_TIMEOUT_MS = 10_000;

function privateParent(socketPath) {
  const parent = path.dirname(socketPath);
  const metadata = fs.lstatSync(parent);
  if (
    !path.isAbsolute(socketPath)
    || !metadata.isDirectory()
    || metadata.isSymbolicLink()
    || metadata.uid !== process.geteuid()
    || (metadata.mode & 0o777) !== 0o700
    || fs.realpathSync(parent) !== path.resolve(parent)
  ) throw new Error('unsafe_control_socket');
}

function safeSocketMetadata(socketPath) {
  const metadata = fs.lstatSync(socketPath);
  return metadata.isSocket()
    && !metadata.isSymbolicLink()
    && metadata.uid === process.geteuid()
    && metadata.nlink === 1
    && (metadata.mode & 0o777) === 0o600;
}

function removeStaleSocket(socketPath) {
  try {
    if (!safeSocketMetadata(socketPath)) throw new Error('unsafe_control_socket');
    fs.unlinkSync(socketPath);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

function encodeFrame(value) {
  const body = Buffer.from(JSON.stringify(value), 'utf8');
  const frame = Buffer.alloc(4 + body.length);
  frame.writeUInt32BE(body.length);
  body.copy(frame, 4);
  return frame;
}

class PairingController {
  constructor({
    stateStore,
    createPairingCode,
    presentPairingCode,
    startPendingBridge,
    stopPendingBridge,
    replaceActiveBridge,
    now = () => Date.now(),
    setTimer = setTimeout,
    clearTimer = clearTimeout,
    presentationTimeoutMs = PRESENTATION_TIMEOUT_MS,
    setPresentationTimer = setTimeout,
    clearPresentationTimer = clearTimeout,
  }) {
    this.stateStore = stateStore;
    this.createPairingCode = createPairingCode;
    this.presentPairingCode = presentPairingCode;
    this.startPendingBridge = startPendingBridge;
    this.stopPendingBridge = stopPendingBridge;
    this.replaceActiveBridge = replaceActiveBridge;
    this.now = now;
    this.setTimer = setTimer;
    this.clearTimer = clearTimer;
    this.presentationTimeoutMs = presentationTimeoutMs;
    this.setPresentationTimer = setPresentationTimer;
    this.clearPresentationTimer = clearPresentationTimer;
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
    this.expiryTimer = null;
    this.removePairingImage = null;
    this.lastState = 'unpaired';
    this.hasTrustedPhone = false;
    this.generationInFlight = false;
  }

  initialize() {
    const state = this.stateStore.loadOrCreate();
    this.hasTrustedPhone = state.trusted_phone !== null;
    if (state.pending_pairing !== null) {
      cancelPendingPairing(this.stateStore, state.pending_pairing.generation);
      this.stopPendingBridge();
      this.lastState = this.hasTrustedPhone ? 'paired' : 'expired';
    } else {
      this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
    }
  }

  status() {
    if (this.activeGeneration !== null) {
      if (this.activeExpiresAtMs <= this.now()) this.expire(this.activeGeneration);
      else if (this.lastState === 'ready') return 'ready';
    }
    return this.lastState;
  }

  async generatePairing() {
    if (this.generationInFlight || this.status() === 'ready') {
      return { ok: false, reason_code: 'pairing_already_active', state: this.status() };
    }
    this.generationInFlight = true;
    let generated = null;
    let failureClass = 'pairing_state_unavailable';
    try {
      this.clearPresentation();
      generated = beginFreshPairing(this.stateStore, this.now());
      this.activeGeneration = generated.generation;
      this.activeExpiresAtMs = generated.pairing_expires_at_ms;
      this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
      failureClass = 'pairing_transport_unavailable';
      this.startPendingBridge(generated);
      failureClass = 'pairing_code_unavailable';
      const pairingCode = this.createPairingCode(generated);
      failureClass = 'pairing_image_unavailable';
      const removePairingImage = await this.presentPairingCodeWithinDeadline(pairingCode);
      failureClass = 'pairing_activation_unavailable';
      if (
        this.activeGeneration !== generated.generation
        || this.activeExpiresAtMs <= this.now()
      ) {
        try { removePairingImage(); } catch {}
        return { ok: true, state: this.status() };
      }
      this.removePairingImage = removePairingImage;
      this.lastState = 'ready';
      const delay = Math.max(0, generated.pairing_expires_at_ms - this.now());
      failureClass = 'pairing_expiry_unavailable';
      this.expiryTimer = this.setTimer(
        () => this.expire(generated.generation),
        delay,
      );
      // Expiry is the security boundary; detaching the already-armed timer is
      // only a process-lifecycle optimization and must not revoke an otherwise
      // valid admission when a managed runtime cannot detach it.
      try { this.expiryTimer?.unref?.(); } catch {}
      return { ok: true, state: 'ready' };
    } catch {
      this.clearPresentation();
      if (generated && this.activeGeneration === generated.generation) {
        try {
          cancelPendingPairing(this.stateStore, generated.generation);
          this.stopPendingBridge();
        } catch {}
        this.activeGeneration = null;
        this.activeExpiresAtMs = 0;
        this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
        return { ok: false, reason_code: failureClass, state: this.lastState };
      }
      if (generated) return { ok: true, state: this.status() };
      return { ok: false, reason_code: failureClass, state: this.status() };
    } finally {
      this.generationInFlight = false;
    }
  }

  presentPairingCodeWithinDeadline(pairingCode) {
    const presentation = Promise.resolve().then(() => this.presentPairingCode(pairingCode));
    return new Promise((resolve, reject) => {
      let timedOut = false;
      const timer = this.setPresentationTimer(() => {
        timedOut = true;
        reject(new Error('pairing_presentation_timeout'));
      }, this.presentationTimeoutMs);
      presentation.then((cleanup) => {
        if (timedOut) {
          try { cleanup(); } catch {}
          return;
        }
        this.clearPresentationTimer(timer);
        resolve(cleanup);
      }, (error) => {
        if (timedOut) return;
        this.clearPresentationTimer(timer);
        reject(error);
      });
    });
  }

  pairingConsumed() {
    this.clearPresentation();
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
    this.hasTrustedPhone = true;
    this.lastState = 'paired';
  }

  cancelPairing() {
    if (this.activeGeneration === null) {
      return { ok: true, state: this.status() };
    }
    if (this.activeExpiresAtMs <= this.now()) return { ok: true, state: this.status() };
    const expectedGeneration = this.activeGeneration;
    this.clearPresentation();
    const state = cancelPendingPairing(this.stateStore, expectedGeneration);
    this.stopPendingBridge();
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
    this.hasTrustedPhone = state.trusted_phone !== null;
    this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
    return { ok: true, state: this.lastState };
  }

  expire(expectedGeneration) {
    if (this.activeGeneration !== expectedGeneration) return;
    try {
      const state = expirePairing(this.stateStore, expectedGeneration, this.now());
      this.stopPendingBridge();
      this.hasTrustedPhone = state.trusted_phone !== null;
      this.lastState = this.hasTrustedPhone ? 'paired' : 'expired';
    } catch {
      this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
    }
    this.clearPresentation();
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
  }

  revoke() {
    this.clearPresentation();
    if (this.activeGeneration !== null) this.stopPendingBridge();
    const state = revokeTrust(this.stateStore);
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
    this.hasTrustedPhone = false;
    this.lastState = 'unpaired';
    this.replaceActiveBridge(state);
    return { ok: true, state: 'unpaired' };
  }

  shutdown() {
    const generation = this.activeGeneration;
    this.clearPresentation();
    this.activeGeneration = null;
    this.activeExpiresAtMs = 0;
    if (generation !== null) {
      try { cancelPendingPairing(this.stateStore, generation); } catch {}
      try { this.stopPendingBridge(); } catch {}
    }
    this.lastState = this.hasTrustedPhone ? 'paired' : 'unpaired';
  }

  clearPresentation() {
    if (this.expiryTimer !== null) this.clearTimer(this.expiryTimer);
    this.expiryTimer = null;
    try { this.removePairingImage?.(); } catch {}
    this.removePairingImage = null;
  }
}

class BridgeControlServer {
  constructor({
    controller,
    socketPath = DEFAULT_CONTROL_SOCKET_PATH,
    inputTimeoutMs = CONTROL_TIMEOUT_MS,
    actionTimeoutMs = CONTROL_ACTION_TIMEOUT_MS,
  }) {
    this.controller = controller;
    this.socketPath = socketPath;
    this.inputTimeoutMs = inputTimeoutMs;
    this.actionTimeoutMs = actionTimeoutMs;
    this.server = null;
  }

  async start() {
    privateParent(this.socketPath);
    removeStaleSocket(this.socketPath);
    const server = net.createServer((socket) => this.handle(socket));
    this.server = server;
    await new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(this.socketPath, () => {
        server.off('error', reject);
        try {
          fs.chmodSync(this.socketPath, 0o600);
          if (!safeSocketMetadata(this.socketPath)) throw new Error('unsafe_control_socket');
          resolve();
        } catch (error) {
          reject(error);
        }
      });
    });
  }

  handle(socket) {
    let buffer = Buffer.alloc(0);
    let handled = false;
    const inputTimer = setTimeout(() => socket.destroy(), this.inputTimeoutMs);
    let actionTimer = null;
    const clearDeadlines = () => {
      clearTimeout(inputTimer);
      if (actionTimer !== null) clearTimeout(actionTimer);
    };
    socket.once('close', clearDeadlines);
    socket.on('error', () => {});
    socket.on('data', async (chunk) => {
      if (handled) return;
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length < 4) return;
      const length = buffer.readUInt32BE(0);
      if (length === 0 || length > MAXIMUM_CONTROL_FRAME || buffer.length > length + 4) {
        handled = true;
        try {
          socket.end(encodeFrame({
            ok: false,
            reason_code: 'invalid_request',
            state: this.controller.status(),
          }));
        } catch { socket.destroy(); }
        return;
      }
      if (buffer.length < length + 4) return;
      handled = true;
      clearTimeout(inputTimer);
      let request;
      try { request = JSON.parse(buffer.subarray(4).toString('utf8')); } catch {}
      let response;
      try {
        if (['generate_pairing', 'cancel_pairing', 'revoke'].includes(request?.command)) {
          actionTimer = setTimeout(() => socket.destroy(), this.actionTimeoutMs);
        }
        if (
          !request
          || typeof request !== 'object'
          || Array.isArray(request)
          || Object.keys(request).length !== 1
          || typeof request.command !== 'string'
        ) {
          response = { ok: false, reason_code: 'invalid_request', state: this.controller.status() };
        } else if (request.command === 'status') {
          response = { ok: true, state: this.controller.status() };
        } else if (request.command === 'generate_pairing') {
          response = await this.controller.generatePairing();
        } else if (request.command === 'cancel_pairing') {
          response = this.controller.cancelPairing();
        } else if (request.command === 'revoke') {
          response = this.controller.revoke();
        } else {
          response = { ok: false, reason_code: 'invalid_request', state: this.controller.status() };
        }
        if (actionTimer !== null) clearTimeout(actionTimer);
        socket.end(encodeFrame(response));
      } catch {
        clearDeadlines();
        socket.destroy();
      }
    });
  }

  async close() {
    const server = this.server;
    this.server = null;
    if (server) await new Promise((resolve) => server.close(resolve));
    try { removeStaleSocket(this.socketPath); } catch {}
  }
}

module.exports = {
  BridgeControlServer,
  DEFAULT_CONTROL_SOCKET_PATH,
  MAXIMUM_CONTROL_FRAME,
  PairingController,
  encodeFrame,
  safeSocketMetadata,
};
