// Handshake/session pattern adapted from Remodex secure-transport.js.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Modified for the MemeX-only protocol and authenticated envelope metadata.
'use strict';

const { createHash, randomBytes, timingSafeEqual } = require('node:crypto');
const {
  KEY_EPOCH,
  PROTOCOL_VERSION,
  assertExactKeys,
  base64url,
  bootstrapProof,
  decodeBase64url,
  decryptEnvelope,
  deriveDirectionalKeys,
  encryptEnvelope,
  generateEphemeral,
  handshakeTranscript,
  identityPrivateKey,
  signTranscript,
  verifyTranscript,
} = require('../protocol');
const { commitPhoneTrust } = require('./state-store');

const CLIENT_HELLO_KEYS = [
  'kind', 'protocol_version', 'session_id', 'pairing_generation', 'handshake_mode', 'phone_device_id',
  'phone_identity_public_key', 'phone_ephemeral_public_key', 'client_nonce',
];
const CLIENT_AUTH_KEYS = ['kind', 'session_id', 'phone_device_id', 'key_epoch', 'phone_signature'];
const BOOTSTRAP_CLIENT_AUTH_KEYS = [...CLIENT_AUTH_KEYS, 'bootstrap_proof'];

class MacSecureSession {
  constructor({
    stateStore, router, send, now = () => Date.now(), onPairingConsumed = () => {},
    authorization = null, concurrent = false, onAsyncError = () => {},
    dispatchBudget = { active: 0 },
  }) {
    this.concurrent = concurrent;
    this.dispatchBudget = dispatchBudget;
    this.epoch = 0;
    this.onAsyncError = onAsyncError;
    this.stateStore = stateStore;
    this.router = router;
    this.send = send;
    this.now = now;
    this.onPairingConsumed = onPairingConsumed;
    this.authorization = authorization;
    this.pending = null;
    this.channel = null;
    this.replyChain = Promise.resolve();
    this.queuedReplies = 0;
    this.sentReplies = [];
  }

  invalidate() {
    this.epoch += 1;
    this.pending = null;
    this.channel = null;
    // Do not release dispatch/response budgets for work that is still running.
  }

  async receive(raw) {
    const bytes = Buffer.isBuffer(raw) ? raw : Buffer.from(raw, 'utf8');
    if (bytes.length === 0 || bytes.length > 384 * 1024) throw new Error('invalid_message');
    let message;
    try { message = JSON.parse(bytes.toString('utf8')); } catch { throw new Error('invalid_message'); }
    if (!this.channel && message.kind === 'client_hello') return this.clientHello(message);
    if (!this.channel && message.kind === 'client_auth') return this.clientAuth(message);
    if (this.channel && message.kind === 'encrypted_envelope') return this.application(message);
    throw new Error('invalid_message');
  }

  clientHello(message) {
    assertExactKeys(message, CLIENT_HELLO_KEYS, 'invalid_handshake');
    const state = this.stateStore.loadOrCreate();
    if (
      message.protocol_version !== PROTOCOL_VERSION
      || typeof message.phone_device_id !== 'string'
      || message.phone_device_id.length > 128
    ) throw new Error('invalid_handshake');
    decodeBase64url(message.phone_identity_public_key, 32, 'invalid_handshake');
    const phoneEphemeral = decodeBase64url(message.phone_ephemeral_public_key, 32, 'invalid_handshake');
    decodeBase64url(message.client_nonce, 32, 'invalid_handshake');
    const requestedBootstrap = message.handshake_mode === 'qr_bootstrap';
    const reconnect = message.handshake_mode === 'trusted_reconnect';
    const exactTrustedPhone = state.trusted_phone
      && state.trusted_phone.phone_device_id === message.phone_device_id
      && state.trusted_phone.phone_identity_public_key === message.phone_identity_public_key;
    const stableMatch = message.session_id === state.session_id
      && message.pairing_generation === state.generation;
    const pendingMatch = state.pending_pairing
      && message.session_id === state.pending_pairing.session_id
      && message.pairing_generation === state.pending_pairing.generation;
    const bootstrap = requestedBootstrap
      && pendingMatch;
    const recovery = requestedBootstrap
      && stableMatch
      && exactTrustedPhone;
    if (
      (!bootstrap && !recovery && !reconnect)
      || (bootstrap && state.pending_pairing.pairing_expires_at_ms <= this.now())
      || (reconnect && (!stableMatch || !exactTrustedPhone))
    ) throw new Error('untrusted_phone');

    const ephemeral = generateEphemeral();
    const fields = {
      protocol_version: PROTOCOL_VERSION,
      session_id: message.session_id,
      pairing_generation: message.pairing_generation,
      handshake_mode: recovery ? 'trusted_recovery' : message.handshake_mode,
      key_epoch: KEY_EPOCH,
      mac_device_id: state.mac_device_id,
      phone_device_id: message.phone_device_id,
      mac_identity_public_key: state.mac_identity_public_key,
      phone_identity_public_key: message.phone_identity_public_key,
      mac_ephemeral_public_key: base64url(ephemeral.publicKey),
      phone_ephemeral_public_key: message.phone_ephemeral_public_key,
      client_nonce: message.client_nonce,
      server_nonce: base64url(randomBytes(32)),
      expires_at_ms: bootstrap ? state.pending_pairing.pairing_expires_at_ms : 0,
    };
    const transcript = handshakeTranscript(fields);
    this.pending = {
      fields,
      transcript,
      ephemeralPrivateKey: ephemeral.privateKey,
      bootstrap,
      bootstrapSecret: bootstrap ? state.pending_pairing.bootstrap_secret : null,
    };
    this.send({
      kind: 'server_hello',
      ...fields,
      mac_signature: signTranscript(identityPrivateKey(state.mac_identity_private_key), transcript),
    });
  }

  clientAuth(message) {
    const pending = this.pending;
    assertExactKeys(
      message,
      pending?.bootstrap ? BOOTSTRAP_CLIENT_AUTH_KEYS : CLIENT_AUTH_KEYS,
      'invalid_handshake',
    );
    const state = this.stateStore.loadOrCreate();
    const pendingPairing = state.pending_pairing;
    if (
      !pending
      || message.session_id !== pending.fields.session_id
      || message.phone_device_id !== pending.fields.phone_device_id
      || message.key_epoch !== KEY_EPOCH
      || (pending.bootstrap && (
        !pendingPairing
        || pendingPairing.session_id !== pending.fields.session_id
        || pendingPairing.generation !== pending.fields.pairing_generation
        || pendingPairing.pairing_expires_at_ms <= this.now()
        || pendingPairing.bootstrap_secret !== pending.bootstrapSecret
      ))
      || (!pending.bootstrap && (
        state.session_id !== pending.fields.session_id
        || state.generation !== pending.fields.pairing_generation
        || !state.trusted_phone
        || state.trusted_phone.phone_device_id !== pending.fields.phone_device_id
        || state.trusted_phone.phone_identity_public_key
          !== pending.fields.phone_identity_public_key
      ))
      || !verifyTranscript(
        decodeBase64url(pending.fields.phone_identity_public_key, 32),
        pending.transcript,
        message.phone_signature,
        true,
      )
    ) throw new Error('invalid_handshake');
    let proof = null;
    if (pending.bootstrap) {
      const presented = decodeBase64url(message.bootstrap_proof, 32, 'invalid_handshake');
      const expected = bootstrapProof(
        pending.bootstrapSecret,
        pending.transcript,
        pending.fields.phone_identity_public_key,
      );
      if (!timingSafeEqual(presented, expected)) throw new Error('invalid_handshake');
      proof = expected;
    }
    const keys = deriveDirectionalKeys(
      pending.ephemeralPrivateKey,
      decodeBase64url(pending.fields.phone_ephemeral_public_key, 32),
      pending.transcript,
      pending.fields,
      proof,
    );
    if (pending.bootstrap) {
      commitPhoneTrust(this.stateStore, {
        generation: pending.fields.pairing_generation,
        sessionId: pending.fields.session_id,
        phoneDeviceId: pending.fields.phone_device_id,
        phoneIdentityPublicKey: pending.fields.phone_identity_public_key,
        now: this.now(),
      });
      try { this.onPairingConsumed(); } catch { /* trust is already durable */ }
    }
    const transcript = pending.transcript;
    this.channel = {
      keys,
      generation: pending.fields.pairing_generation,
      sessionId: pending.fields.session_id,
      inboundCounter: 0n,
      outboundCounter: 0n,
      phoneDeviceId: pending.fields.phone_device_id,
      phoneIdentityPublicKey: pending.fields.phone_identity_public_key,
    };
    this.pending = null;
    const ready = Buffer.from(JSON.stringify({
      type: 'handshake_ready',
      transcript_hash: base64url(createHash('sha256').update(transcript).digest()),
    }), 'utf8');
    this.sendEncrypted(ready);
  }

  async application(envelope) {
    const channel = this.channel;
    this.assertTrustedChannel(channel, () => {});
    const grant = this.authorization?.snapshot(channel);
    const { plaintext } = decryptEnvelope({
      key: channel.keys.phoneToMac,
      envelope,
      sessionId: channel.sessionId,
      sender: 'iphone',
      expectedCounter: channel.inboundCounter,
    });
    channel.inboundCounter += 1n;
    // Only authenticated channel identity reaches scoped capture ownership.
    const principal = createHash('sha256').update(JSON.stringify([
      channel.sessionId, channel.generation, channel.phoneDeviceId, channel.phoneIdentityPublicKey,
    ])).digest('hex');
    // Decrypt in order, but answer each request when it is ready: responses carry
    // their request id, so one slow daemon call no longer blocks the ones behind it.
    if (this.dispatchBudget.active >= 16) throw new Error('reply_backpressure');
    this.dispatchBudget.active += 1;
    const epoch = this.epoch;
    const dispatched = Promise.resolve().then(() => {
      if (this.epoch !== epoch || this.channel !== channel) return;
      this.assertTrustedChannel(channel, () => this.authorization?.assertCurrent(channel, grant));
      return this.router(plaintext, grant, principal).then((response) => {
        if (this.epoch === epoch && this.channel === channel) return this.reply(channel, grant, response);
      });
    }).finally(() => { this.dispatchBudget.active -= 1; });
    if (!this.concurrent) return dispatched;
    dispatched.catch((error) => {
      if (this.epoch === epoch && this.channel === channel) this.onAsyncError(error);
    });
    return undefined;
  }

  reply(channel, grant, response) {
    // All concurrent RPCs share this budget. Measure encrypted wire bytes,
    // not the inner JSON; keep headroom beneath the relay's 768KiB/s and
    // 30-frame limits for handshake/control traffic. Bound queued plaintext.
    if (this.queuedReplies >= 16) return Promise.reject(new Error('reply_backpressure'));
    this.queuedReplies += 1;
    const plaintext = Buffer.from(JSON.stringify(response), 'utf8');
    const run = this.replyChain.then(async () => {
      if (this.channel !== channel) return;
      const envelope = encryptEnvelope({ key: channel.keys.macToPhone, sessionId: channel.sessionId,
        sender: 'mac', counter: channel.outboundCounter, plaintext });
      const size = Buffer.byteLength(JSON.stringify(envelope));
      if (size > 512 * 1024) throw new Error('reply_too_large');
      while (true) {
        if (this.channel !== channel) return;
        const now = Date.now();
        this.sentReplies = this.sentReplies.filter(r => now - r.at < 1010);
        if (this.sentReplies.length < 16 && this.sentReplies.reduce((n, r) => n + r.size, 0) + size <= 512 * 1024) break;
        await new Promise(resolve => setTimeout(resolve, Math.max(1, this.sentReplies[0].at + 1010 - now)));
      }
      // Never authorize only at enqueue time: expiry/revocation may occur
      // while a large image waits behind another reply.
      if (this.channel !== channel) return;
      this.assertTrustedChannel(channel, () => {
        if (this.channel !== channel) throw new Error('untrusted_phone');
        this.authorization?.assertCurrent(channel, grant);
        channel.outboundCounter += 1n;
        this.send(envelope);
        this.sentReplies.push({ at: Date.now(), size });
      });
    });
    this.replyChain = run.catch(() => {});
    return run.finally(() => { this.queuedReplies -= 1; });
  }

  assertTrustedChannel(channel, operation) {
    if (!channel || this.channel !== channel) throw new Error('untrusted_phone');
    try {
      return this.stateStore.withTrustedChannel({
        generation: channel.generation,
        sessionId: channel.sessionId,
        phoneDeviceId: channel.phoneDeviceId,
        phoneIdentityPublicKey: channel.phoneIdentityPublicKey,
      }, operation);
    } catch (error) {
      if (this.channel === channel) this.channel = null;
      throw error;
    }
  }

  sendEncrypted(plaintext) {
    const channel = this.channel;
    const envelope = encryptEnvelope({
      key: channel.keys.macToPhone,
      sessionId: channel.sessionId,
      sender: 'mac',
      counter: channel.outboundCounter,
      plaintext,
    });
    channel.outboundCounter += 1n;
    this.send(envelope);
  }
}

module.exports = { MacSecureSession };
