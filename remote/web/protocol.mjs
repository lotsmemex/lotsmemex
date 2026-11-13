// Secure transport model adapted from Remodex iOS secure transport slices.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Substantially modified for native WebCrypto, authenticated metadata, and MemeX-only RPC.
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder('utf-8', { fatal: true });

export const FIXED_SUBPROTOCOL = 'memex.remote.v1';
export const KEY_EPOCH = 1;
export const MAX_PLAINTEXT_BYTES = 256 * 1024;
export const PROTOCOL_VERSION = 1;
export const SOCKET_TIMEOUT_MS = 10_000;
export const BROWSER_PAIRING_PREFIX = 'MEMEX PAIR 1:';

const HANDSHAKE_DOMAIN = 'memex-e2ee-v1';
const BOOTSTRAP_PROOF_DOMAIN = 'memex-bootstrap-proof-v1';
const KDF_BINDING_DOMAIN = 'memex-kdf-binding-v1';
const ENVELOPE_DOMAIN = 'memex-envelope-v1';
const PAIRING_KEYS = [
  'v', 'web_client_url', 'relay', 'session_id', 'pairing_generation', 'mac_device_id',
  'mac_identity_public_key', 'bootstrap_secret', 'iphone_admission_capability',
  'expires_at_ms',
];
const CLIENT_HELLO_KEYS = [
  'kind', 'protocol_version', 'session_id', 'pairing_generation', 'handshake_mode',
  'phone_device_id', 'phone_identity_public_key', 'phone_ephemeral_public_key',
  'client_nonce',
];
const SERVER_HELLO_KEYS = [
  'kind', 'protocol_version', 'session_id', 'pairing_generation', 'handshake_mode',
  'key_epoch', 'mac_device_id', 'phone_device_id', 'mac_identity_public_key',
  'phone_identity_public_key', 'mac_ephemeral_public_key',
  'phone_ephemeral_public_key', 'client_nonce', 'server_nonce', 'expires_at_ms',
  'mac_signature',
];
const ENVELOPE_KEYS = [
  'kind', 'v', 'session_id', 'key_epoch', 'sender', 'counter', 'ciphertext', 'tag',
];

function bytes(value) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new Error('invalid_bytes');
}

function utf8(value) {
  return textEncoder.encode(String(value));
}

export function concatBytes(parts) {
  const values = parts.map(bytes);
  const length = values.reduce((sum, value) => sum + value.length, 0);
  const output = new Uint8Array(length);
  let offset = 0;
  for (const value of values) {
    output.set(value, offset);
    offset += value.length;
  }
  return output;
}

export function base64url(value) {
  const input = bytes(value);
  let binary = '';
  for (let offset = 0; offset < input.length; offset += 0x8000) {
    binary += String.fromCharCode(...input.subarray(offset, offset + 0x8000));
  }
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

export function decodeBase64url(value, expectedLength = null, code = 'invalid_base64url') {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error(code);
  let binary;
  try {
    const standard = value.replaceAll('-', '+').replaceAll('_', '/');
    binary = atob(standard + '='.repeat((4 - (standard.length % 4)) % 4));
  } catch {
    throw new Error(code);
  }
  const output = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  if (base64url(output) !== value || (expectedLength !== null && output.length !== expectedLength)) {
    throw new Error(code);
  }
  return output;
}

export function assertExactKeys(value, expected, code = 'invalid_message') {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(code);
  const actual = Object.keys(value).sort();
  const keys = [...expected].sort();
  if (actual.length !== keys.length || actual.some((key, index) => key !== keys[index])) {
    throw new Error(code);
  }
}

export function lengthPrefixed(parts) {
  const encoded = parts.map((part) => (
    typeof part === 'string' || typeof part === 'number' || typeof part === 'bigint'
      ? utf8(part)
      : bytes(part)
  ));
  const output = new Uint8Array(encoded.reduce((sum, part) => sum + 4 + part.length, 0));
  const view = new DataView(output.buffer);
  let offset = 0;
  for (const part of encoded) {
    view.setUint32(offset, part.length, false);
    offset += 4;
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

export function validatedRelay(value, { allowLocal = false } = {}) {
  let relay;
  try { relay = new URL(value); } catch { throw new Error('invalid_pairing'); }
  const local = allowLocal && relay.protocol === 'ws:'
    && ['localhost', '127.0.0.1', '::1'].includes(relay.hostname);
  if (
    (relay.protocol !== 'wss:' && !local)
    || relay.username || relay.password || (!local && relay.port) || relay.search || relay.hash
    || relay.pathname !== '/relay' || relay.toString() !== value
  ) throw new Error('invalid_pairing');
  relay.pathname = '/relay';
  return relay;
}

function validatedWebClient(value, { allowLocal = false } = {}) {
  let client;
  try { client = new URL(value); } catch { throw new Error('invalid_pairing'); }
  const local = allowLocal && client.protocol === 'http:'
    && ['localhost', '127.0.0.1', '::1'].includes(client.hostname);
  if (
    (client.protocol !== 'https:' && !local)
    || client.username || client.password || (!local && client.port) || client.search || client.hash
    || !client.pathname.startsWith('/') || client.pathname.includes('//')
    || client.pathname.endsWith('/') || client.toString() !== value
  ) throw new Error('invalid_pairing');
  return client;
}

function validatePairingObject(value, pageHref, now, persisted, config) {
  const { webClientUrl, relayUrl, allowLocal = false } = config;
  assertExactKeys(value, persisted ? [...PAIRING_KEYS, 'trusted'] : PAIRING_KEYS, 'invalid_pairing');
  if (persisted && value.trusted !== true) throw new Error('invalid_pairing');
  const relay = validatedRelay(value.relay, { allowLocal });
  const expectedRelay = validatedRelay(relayUrl, { allowLocal });
  const pairedPage = validatedWebClient(value.web_client_url, { allowLocal });
  const expectedPage = validatedWebClient(webClientUrl, { allowLocal });
  const page = new URL(pageHref);
  const secondaryBase = expectedPage.pathname === '/' ? '' : expectedPage.pathname;
  const allowedPaths = new Set([
    expectedPage.pathname,
    `${secondaryBase}/settings`,
    `${secondaryBase}/account`,
  ]);
  if (
    page.origin !== expectedPage.origin || !allowedPaths.has(page.pathname)
    || page.search || page.hash
    || pairedPage.toString() !== expectedPage.toString()
    || relay.toString() !== expectedRelay.toString()
    || value.v !== 1
    || !Number.isSafeInteger(value.pairing_generation)
    || value.pairing_generation < 1
    || typeof value.mac_device_id !== 'string'
    || value.mac_device_id.length === 0
    || value.mac_device_id.length > 128
    || !Number.isSafeInteger(value.expires_at_ms)
    || value.expires_at_ms < 0
    || (persisted && typeof value.trusted !== 'boolean')
  ) throw new Error('invalid_pairing');
  decodeBase64url(value.session_id, 32, 'invalid_pairing');
  decodeBase64url(value.mac_identity_public_key, 32, 'invalid_pairing');
  decodeBase64url(value.iphone_admission_capability, 32, 'invalid_pairing');
  if (value.bootstrap_secret === null) {
    if (!persisted || !value.trusted) throw new Error('invalid_pairing');
  } else {
    decodeBase64url(value.bootstrap_secret, 32, 'invalid_pairing');
  }
  if (!persisted) {
    const distance = value.expires_at_ms - now;
    if (distance < -30_000 || distance > (30 * 60_000) + 30_000) throw new Error('invalid_pairing');
  }
  return {
    ...value,
    relay: relay.toString(),
    trusted: persisted ? value.trusted : false,
  };
}

export function parsePairingCode(code, pageHref, config, now = Date.now()) {
  if (
    typeof code !== 'string'
    || code.length > 4_096
    || !code.startsWith(BROWSER_PAIRING_PREFIX)
  ) throw new Error('invalid_pairing');
  const payload = decodeBase64url(
    code.slice(BROWSER_PAIRING_PREFIX.length), null, 'invalid_pairing',
  );
  if (payload.length === 0 || payload.length > 3_072) throw new Error('invalid_pairing');
  let value;
  try { value = JSON.parse(textDecoder.decode(payload)); } catch { throw new Error('invalid_pairing'); }
  return validatePairingObject(value, pageHref, now, false, config);
}

export function validatePersistedPairing(value, pageHref, config) {
  return validatePairingObject(value, pageHref, Date.now(), true, config);
}

export function relaySocketUrl(pairing, { allowLocal = false } = {}) {
  const relay = validatedRelay(pairing.relay, { allowLocal });
  decodeBase64url(pairing.session_id, 32, 'invalid_pairing');
  relay.pathname = `/relay/${pairing.session_id}`;
  return relay.toString();
}

export function browserSubprotocols(pairing) {
  decodeBase64url(pairing.iphone_admission_capability, 32, 'invalid_pairing');
  return [
    FIXED_SUBPROTOCOL,
    'memex.role.iphone',
    'memex.client.browser',
    `memex.cap.${pairing.iphone_admission_capability}`,
  ];
}

export function handshakeTranscript(fields) {
  if (
    !['qr_bootstrap', 'trusted_recovery', 'trusted_reconnect'].includes(fields.handshake_mode)
    || fields.protocol_version !== PROTOCOL_VERSION
    || fields.key_epoch !== KEY_EPOCH
    || !Number.isSafeInteger(fields.pairing_generation)
    || fields.pairing_generation < 1
    || !Number.isSafeInteger(fields.expires_at_ms)
    || fields.expires_at_ms < 0
  ) throw new Error('invalid_handshake');
  return lengthPrefixed([
    HANDSHAKE_DOMAIN,
    fields.session_id,
    fields.pairing_generation,
    fields.protocol_version,
    fields.handshake_mode,
    fields.key_epoch,
    fields.mac_device_id,
    fields.phone_device_id,
    decodeBase64url(fields.mac_identity_public_key, 32, 'invalid_handshake'),
    decodeBase64url(fields.phone_identity_public_key, 32, 'invalid_handshake'),
    decodeBase64url(fields.mac_ephemeral_public_key, 32, 'invalid_handshake'),
    decodeBase64url(fields.phone_ephemeral_public_key, 32, 'invalid_handshake'),
    decodeBase64url(fields.client_nonce, 32, 'invalid_handshake'),
    decodeBase64url(fields.server_nonce, 32, 'invalid_handshake'),
    fields.expires_at_ms,
  ]);
}

export async function sha256(input, subtle = globalThis.crypto.subtle) {
  return new Uint8Array(await subtle.digest('SHA-256', bytes(input)));
}

export async function bootstrapProof(
  secret,
  transcript,
  phoneIdentityPublicKey,
  subtle = globalThis.crypto.subtle,
) {
  const secretBytes = typeof secret === 'string'
    ? decodeBase64url(secret, 32, 'invalid_bootstrap_secret')
    : bytes(secret);
  const publicBytes = typeof phoneIdentityPublicKey === 'string'
    ? decodeBase64url(phoneIdentityPublicKey, 32, 'invalid_public_key')
    : bytes(phoneIdentityPublicKey);
  if (secretBytes.length !== 32 || publicBytes.length !== 32) throw new Error('invalid_handshake');
  const key = await subtle.importKey('raw', secretBytes, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  return new Uint8Array(await subtle.sign('HMAC', key, lengthPrefixed([
    BOOTSTRAP_PROOF_DOMAIN,
    transcript,
    publicBytes,
  ])));
}

export function signingInput(transcript, clientAuth = false) {
  return clientAuth
    ? concatBytes([transcript, lengthPrefixed(['client-auth'])])
    : bytes(transcript);
}

export async function generateIdentity(subtle = globalThis.crypto.subtle) {
  const pair = await subtle.generateKey({ name: 'Ed25519' }, false, ['sign', 'verify']);
  if (pair.privateKey.extractable || pair.privateKey.usages.join(',') !== 'sign') {
    throw new Error('unsupported_browser');
  }
  const publicKeyRaw = new Uint8Array(await subtle.exportKey('raw', pair.publicKey));
  if (publicKeyRaw.length !== 32) throw new Error('unsupported_browser');
  return { privateKey: pair.privateKey, publicKey: base64url(publicKeyRaw) };
}

export async function verifyStoredIdentity(identity, subtle = globalThis.crypto.subtle) {
  assertExactKeys(identity, ['privateKey', 'publicKey'], 'storage_unavailable');
  if (
    !identity.privateKey
    || identity.privateKey.type !== 'private'
    || identity.privateKey.extractable
    || identity.privateKey.algorithm?.name !== 'Ed25519'
    || !identity.privateKey.usages.includes('sign')
  ) throw new Error('storage_unavailable');
  const publicBytes = decodeBase64url(identity.publicKey, 32, 'storage_unavailable');
  const publicKey = await subtle.importKey('raw', publicBytes, 'Ed25519', true, ['verify']);
  const challenge = utf8('memex-browser-identity-check-v1');
  const signature = await subtle.sign('Ed25519', identity.privateKey, challenge);
  if (!await subtle.verify('Ed25519', publicKey, signature, challenge)) {
    throw new Error('storage_unavailable');
  }
  return identity;
}

export async function signTranscript(identityPrivateKey, transcript, subtle = globalThis.crypto.subtle) {
  return new Uint8Array(await subtle.sign(
    'Ed25519', identityPrivateKey, signingInput(transcript, true),
  ));
}

export async function verifyTranscript(
  publicKeyRaw,
  transcript,
  signature,
  clientAuth = false,
  subtle = globalThis.crypto.subtle,
) {
  const publicKey = await subtle.importKey('raw', bytes(publicKeyRaw), 'Ed25519', true, ['verify']);
  return subtle.verify('Ed25519', publicKey, bytes(signature), signingInput(transcript, clientAuth));
}

export async function generateEphemeral(subtle = globalThis.crypto.subtle) {
  const pair = await subtle.generateKey({ name: 'X25519' }, false, ['deriveBits']);
  if (pair.privateKey.extractable || !pair.privateKey.usages.includes('deriveBits')) {
    throw new Error('unsupported_browser');
  }
  const publicKeyRaw = new Uint8Array(await subtle.exportKey('raw', pair.publicKey));
  if (publicKeyRaw.length !== 32) throw new Error('unsupported_browser');
  return { privateKey: pair.privateKey, publicKeyRaw };
}

export async function deriveDirectionalKeyBytes(
  localPrivateKey,
  peerPublicKeyRaw,
  transcript,
  fields,
  bootstrapProofBytes = null,
  subtle = globalThis.crypto.subtle,
) {
  const peer = await subtle.importKey('raw', bytes(peerPublicKeyRaw), 'X25519', true, []);
  const shared = new Uint8Array(await subtle.deriveBits(
    { name: 'X25519', public: peer }, localPrivateKey, 256,
  ));
  let binding;
  if (fields.handshake_mode === 'qr_bootstrap') {
    binding = bytes(bootstrapProofBytes);
    if (binding.length !== 32) throw new Error('invalid_handshake');
  } else {
    if (bootstrapProofBytes !== null) throw new Error('invalid_handshake');
    binding = utf8(fields.handshake_mode);
  }
  const salt = await sha256(lengthPrefixed([
    KDF_BINDING_DOMAIN,
    transcript,
    binding,
  ]), subtle);
  const source = await subtle.importKey('raw', shared, 'HKDF', false, ['deriveBits']);
  const prefix = `${HANDSHAKE_DOMAIN}|${fields.session_id}|${fields.mac_device_id}|${fields.phone_device_id}|${fields.key_epoch}`;
  const derive = async (direction) => new Uint8Array(await subtle.deriveBits({
    name: 'HKDF',
    hash: 'SHA-256',
    salt,
    info: utf8(`${prefix}|${direction}`),
  }, source, 256));
  return {
    phoneToMac: await derive('phoneToMac'),
    macToPhone: await derive('macToPhone'),
  };
}

export async function importDirectionalKeys(keyBytes, subtle = globalThis.crypto.subtle) {
  return {
    phoneToMac: await subtle.importKey('raw', keyBytes.phoneToMac, 'AES-GCM', false, ['encrypt']),
    macToPhone: await subtle.importKey('raw', keyBytes.macToPhone, 'AES-GCM', false, ['decrypt']),
  };
}

export function parseCounter(value) {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value)) {
    throw new Error('invalid_counter');
  }
  const counter = BigInt(value);
  if (counter > 0xffffffffffffffffn) throw new Error('invalid_counter');
  return counter;
}

function nonceFor(sender, counter) {
  if (!['mac', 'iphone'].includes(sender)) throw new Error('invalid_sender');
  const value = typeof counter === 'bigint' ? counter : parseCounter(counter);
  const nonce = new Uint8Array(12);
  nonce[0] = sender === 'mac' ? 1 : 2;
  new DataView(nonce.buffer).setBigUint64(4, value, false);
  return nonce;
}

function envelopeAad(fields) {
  return lengthPrefixed([
    ENVELOPE_DOMAIN,
    fields.session_id,
    fields.v,
    fields.key_epoch,
    fields.sender,
    fields.counter,
  ]);
}

export async function encryptEnvelope({
  key, sessionId, sender, counter, plaintext, subtle = globalThis.crypto.subtle,
}) {
  const clear = typeof plaintext === 'string' ? utf8(plaintext) : bytes(plaintext);
  if (clear.length === 0 || clear.length > MAX_PLAINTEXT_BYTES) throw new Error('payload_too_large');
  const counterText = String(counter);
  parseCounter(counterText);
  const header = {
    kind: 'encrypted_envelope',
    v: PROTOCOL_VERSION,
    session_id: sessionId,
    key_epoch: KEY_EPOCH,
    sender,
    counter: counterText,
  };
  const sealed = new Uint8Array(await subtle.encrypt({
    name: 'AES-GCM',
    iv: nonceFor(sender, counterText),
    additionalData: envelopeAad(header),
    tagLength: 128,
  }, key, clear));
  return {
    ...header,
    ciphertext: base64url(sealed.subarray(0, -16)),
    tag: base64url(sealed.subarray(-16)),
  };
}

export async function decryptEnvelope({
  key, envelope, sessionId, sender, expectedCounter, subtle = globalThis.crypto.subtle,
}) {
  assertExactKeys(envelope, ENVELOPE_KEYS, 'invalid_envelope');
  if (
    envelope.kind !== 'encrypted_envelope'
    || envelope.v !== PROTOCOL_VERSION
    || envelope.session_id !== sessionId
    || envelope.key_epoch !== KEY_EPOCH
    || envelope.sender !== sender
    || parseCounter(envelope.counter) !== expectedCounter
  ) throw new Error('invalid_envelope');
  const ciphertext = decodeBase64url(envelope.ciphertext, null, 'invalid_envelope');
  const tag = decodeBase64url(envelope.tag, 16, 'invalid_envelope');
  if (ciphertext.length === 0 || ciphertext.length > MAX_PLAINTEXT_BYTES) {
    throw new Error('invalid_envelope');
  }
  try {
    const plaintext = new Uint8Array(await subtle.decrypt({
      name: 'AES-GCM',
      iv: nonceFor(sender, envelope.counter),
      additionalData: envelopeAad(envelope),
      tagLength: 128,
    }, key, concatBytes([ciphertext, tag])));
    if (plaintext.length === 0 || plaintext.length > MAX_PLAINTEXT_BYTES) {
      throw new Error('invalid_envelope');
    }
    return plaintext;
  } catch (error) {
    if (error?.message === 'invalid_envelope') throw error;
    throw new Error('invalid_envelope');
  }
}

const MAX_IN_FLIGHT = 8;

export class WebSecureSession {
  constructor({ pairing, identity, phoneDeviceId, send, onTrust, onReady, subtle }) {
    this.pairing = pairing;
    this.identity = identity;
    this.phoneDeviceId = phoneDeviceId;
    this.send = send;
    this.onTrust = onTrust;
    this.onReady = onReady;
    this.subtle = subtle || globalThis.crypto.subtle;
    this.ephemeral = null;
    this.clientNonce = null;
    this.transcriptHash = null;
    this.keys = null;
    this.inboundCounter = 0n;
    this.outboundCounter = 0n;
    this.ready = false;
    this.waiting = new Map();
  }

  async start(random = globalThis.crypto) {
    this.ephemeral = await generateEphemeral(this.subtle);
    const nonce = random.getRandomValues(new Uint8Array(32));
    this.clientNonce = base64url(nonce);
    const hello = {
      kind: 'client_hello',
      protocol_version: PROTOCOL_VERSION,
      session_id: this.pairing.session_id,
      pairing_generation: this.pairing.pairing_generation,
      handshake_mode: this.pairing.trusted ? 'trusted_reconnect' : 'qr_bootstrap',
      phone_device_id: this.phoneDeviceId,
      phone_identity_public_key: this.identity.publicKey,
      phone_ephemeral_public_key: base64url(this.ephemeral.publicKeyRaw),
      client_nonce: this.clientNonce,
    };
    assertExactKeys(hello, CLIENT_HELLO_KEYS);
    this.send(JSON.stringify(hello));
  }

  async receive(raw) {
    const encoded = typeof raw === 'string' ? utf8(raw) : bytes(raw);
    if (encoded.length === 0 || encoded.length > 384 * 1024) throw new Error('invalid_message');
    let message;
    try { message = JSON.parse(textDecoder.decode(encoded)); } catch { throw new Error('invalid_message'); }
    if (!this.keys && message.kind === 'server_hello') return this.receiveServerHello(message);
    if (this.keys && message.kind === 'encrypted_envelope') return this.receiveEncrypted(message);
    throw new Error('invalid_message');
  }

  async receiveServerHello(message) {
    assertExactKeys(message, SERVER_HELLO_KEYS, 'invalid_handshake');
    const { kind: _kind, mac_signature: signature, ...fields } = message;
    const expectedMode = this.pairing.trusted
      ? fields.handshake_mode === 'trusted_reconnect'
      : ['qr_bootstrap', 'trusted_recovery'].includes(fields.handshake_mode);
    if (
      fields.session_id !== this.pairing.session_id
      || fields.pairing_generation !== this.pairing.pairing_generation
      || fields.mac_device_id !== this.pairing.mac_device_id
      || fields.mac_identity_public_key !== this.pairing.mac_identity_public_key
      || fields.phone_device_id !== this.phoneDeviceId
      || fields.phone_identity_public_key !== this.identity.publicKey
      || fields.phone_ephemeral_public_key !== base64url(this.ephemeral.publicKeyRaw)
      || fields.client_nonce !== this.clientNonce
      || !expectedMode
      || fields.expires_at_ms !== (
        fields.handshake_mode === 'qr_bootstrap' ? this.pairing.expires_at_ms : 0
      )
    ) throw new Error('invalid_handshake');
    const transcript = handshakeTranscript(fields);
    if (!await verifyTranscript(
      decodeBase64url(this.pairing.mac_identity_public_key, 32, 'invalid_handshake'),
      transcript,
      decodeBase64url(signature, 64, 'invalid_handshake'),
      false,
      this.subtle,
    )) throw new Error('invalid_handshake');
    const proof = fields.handshake_mode === 'qr_bootstrap'
      ? await bootstrapProof(
        this.pairing.bootstrap_secret,
        transcript,
        decodeBase64url(this.identity.publicKey, 32),
        this.subtle,
      )
      : null;
    const keyBytes = await deriveDirectionalKeyBytes(
      this.ephemeral.privateKey,
      decodeBase64url(fields.mac_ephemeral_public_key, 32, 'invalid_handshake'),
      transcript,
      fields,
      proof,
      this.subtle,
    );
    this.keys = await importDirectionalKeys(keyBytes, this.subtle);
    this.transcriptHash = base64url(await sha256(transcript, this.subtle));
    const auth = {
      kind: 'client_auth',
      session_id: this.pairing.session_id,
      phone_device_id: this.phoneDeviceId,
      key_epoch: KEY_EPOCH,
      phone_signature: base64url(await signTranscript(
        this.identity.privateKey, transcript, this.subtle,
      )),
    };
    if (proof) auth.bootstrap_proof = base64url(proof);
    this.send(JSON.stringify(auth));
  }

  async receiveEncrypted(message) {
    const plaintext = await decryptEnvelope({
      key: this.keys.macToPhone,
      envelope: message,
      sessionId: this.pairing.session_id,
      sender: 'mac',
      expectedCounter: this.inboundCounter,
      subtle: this.subtle,
    });
    this.inboundCounter += 1n;
    let response;
    try { response = JSON.parse(textDecoder.decode(plaintext)); } catch { throw new Error('invalid_message'); }
    if (!this.ready && response.type === 'handshake_ready') {
      assertExactKeys(response, ['type', 'transcript_hash'], 'invalid_handshake');
      if (response.transcript_hash !== this.transcriptHash) throw new Error('invalid_handshake');
      const trusted = {
        ...this.pairing,
        trusted: true,
        bootstrap_secret: null,
        expires_at_ms: 0,
      };
      await this.onTrust(trusted);
      this.pairing = trusted;
      this.ready = true;
      this.ephemeral = null;
      await this.onReady();
      return;
    }
    if (!this.ready) throw new Error('invalid_handshake');
    if (!response || typeof response !== 'object' || Array.isArray(response)) {
      throw new Error('invalid_message');
    }
    const id = response.id;
    if (typeof id !== 'string' || !/^[A-Za-z0-9_.-]{1,128}$/.test(id)) {
      throw new Error('invalid_message');
    }
    const pending = this.waiting.get(id);
    if (!pending) throw new Error('invalid_message');
    this.waiting.delete(id);
    pending.resolve(response);
  }

  async request(id, method, params) {
    // Several requests may be in flight; the Mac answers each by its unique id.
    if (!this.ready || this.waiting.has(id) || this.waiting.size >= MAX_IN_FLIGHT) {
      throw new Error('transport_unavailable');
    }
    const request = { id, method, params };
    const response = new Promise((resolve, reject) => this.waiting.set(id, { resolve, reject }));
    // Encryption is async: take the counter and keep send order before awaiting, so two
    // concurrent requests never share a counter or reach the Mac out of order.
    const counter = this.outboundCounter;
    this.outboundCounter += 1n;
    const previous = this.sendChain || Promise.resolve();
    const encrypted = encryptEnvelope({
      key: this.keys.phoneToMac,
      sessionId: this.pairing.session_id,
      sender: 'iphone',
      counter,
      plaintext: JSON.stringify(request),
      subtle: this.subtle,
    });
    const sent = previous.then(() => encrypted).then((envelope) => this.send(JSON.stringify(envelope)));
    this.sendChain = sent.catch(() => {});
    try {
      await sent;
      return response;
    } catch (error) {
      this.waiting.delete(id);
      throw error;
    }
  }

  close() {
    for (const pending of this.waiting.values()) pending.reject(new Error('transport_unavailable'));
    this.waiting.clear();
    this.ephemeral = null;
    this.keys = null;
    this.ready = false;
  }
}
