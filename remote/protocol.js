// Portions of the transport design are adapted from Remodex secure-transport.js.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Substantially modified for MemeX: authenticated envelope metadata, strict
// counters/schemas, a smaller handshake, and MemeX-only RPC payloads.
'use strict';

const {
  createCipheriv,
  createDecipheriv,
  createHash,
  createHmac,
  createPrivateKey,
  createPublicKey,
  diffieHellman,
  generateKeyPairSync,
  hkdfSync,
  sign,
  verify,
} = require('node:crypto');

const PROTOCOL_VERSION = 1;
const KEY_EPOCH = 1;
const HANDSHAKE_DOMAIN = 'memex-e2ee-v1';
const BOOTSTRAP_PROOF_DOMAIN = 'memex-bootstrap-proof-v1';
const KDF_BINDING_DOMAIN = 'memex-kdf-binding-v1';
const ENVELOPE_DOMAIN = 'memex-envelope-v1';
const MAX_PLAINTEXT_BYTES = 256 * 1024;
const ED25519_PKCS8_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const X25519_PKCS8_PREFIX = Buffer.from('302e020100300506032b656e04220420', 'hex');

function base64url(bytes) {
  return Buffer.from(bytes).toString('base64url');
}

function decodeBase64url(value, expectedLength, field = 'invalid_base64url') {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new Error(field);
  }
  const decoded = Buffer.from(value, 'base64url');
  if (base64url(decoded) !== value || decoded.length !== expectedLength) {
    throw new Error(field);
  }
  return decoded;
}

function assertExactKeys(value, keys, code = 'invalid_message') {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(code);
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(code);
  }
}

function lengthPrefixed(parts) {
  const output = [];
  for (const part of parts) {
    const bytes = Buffer.isBuffer(part) ? part : Buffer.from(String(part), 'utf8');
    if (bytes.length > 0xffffffff) throw new Error('value_too_large');
    const length = Buffer.alloc(4);
    length.writeUInt32BE(bytes.length);
    output.push(length, bytes);
  }
  return Buffer.concat(output);
}

function privateKeyFromSeed(kind, seed) {
  const bytes = Buffer.from(seed);
  if (bytes.length !== 32) throw new Error('invalid_private_key');
  const prefix = kind === 'ed25519' ? ED25519_PKCS8_PREFIX : X25519_PKCS8_PREFIX;
  return createPrivateKey({ key: Buffer.concat([prefix, bytes]), format: 'der', type: 'pkcs8' });
}

function rawPublicKey(key) {
  const der = createPublicKey(key).export({ format: 'der', type: 'spki' });
  return Buffer.from(der).subarray(-32);
}

function publicKeyFromRaw(kind, raw) {
  const bytes = Buffer.from(raw);
  if (bytes.length !== 32) throw new Error('invalid_public_key');
  const algorithm = kind === 'ed25519' ? '6570' : '656e';
  const prefix = Buffer.from(`302a300506032b${algorithm}032100`, 'hex');
  return createPublicKey({ key: Buffer.concat([prefix, bytes]), format: 'der', type: 'spki' });
}

function generateIdentity() {
  const { privateKey } = generateKeyPairSync('ed25519');
  return {
    private_key_pkcs8: base64url(privateKey.export({ format: 'der', type: 'pkcs8' })),
    public_key: base64url(rawPublicKey(privateKey)),
  };
}

function identityPrivateKey(encoded) {
  const bytes = Buffer.from(encoded, 'base64url');
  if (base64url(bytes) !== encoded || bytes.length !== 48) throw new Error('invalid_private_key');
  return createPrivateKey({ key: bytes, format: 'der', type: 'pkcs8' });
}

function generateEphemeral() {
  const { privateKey } = generateKeyPairSync('x25519');
  return { privateKey, publicKey: rawPublicKey(privateKey) };
}

function handshakeTranscript(fields) {
  const mode = fields.handshake_mode;
  if (!['qr_bootstrap', 'trusted_recovery', 'trusted_reconnect'].includes(mode)) {
    throw new Error('invalid_handshake');
  }
  if (!Number.isSafeInteger(fields.pairing_generation) || fields.pairing_generation < 1) {
    throw new Error('invalid_handshake');
  }
  return lengthPrefixed([
    HANDSHAKE_DOMAIN,
    fields.session_id,
    String(fields.pairing_generation),
    String(fields.protocol_version),
    mode,
    String(fields.key_epoch),
    fields.mac_device_id,
    fields.phone_device_id,
    decodeBase64url(fields.mac_identity_public_key, 32),
    decodeBase64url(fields.phone_identity_public_key, 32),
    decodeBase64url(fields.mac_ephemeral_public_key, 32),
    decodeBase64url(fields.phone_ephemeral_public_key, 32),
    decodeBase64url(fields.client_nonce, 32),
    decodeBase64url(fields.server_nonce, 32),
    String(fields.expires_at_ms),
  ]);
}

function bootstrapProof(secret, transcript, phoneIdentityPublicKey) {
  const key = Buffer.isBuffer(secret)
    ? Buffer.from(secret)
    : decodeBase64url(secret, 32, 'invalid_bootstrap_secret');
  if (key.length !== 32) throw new Error('invalid_bootstrap_secret');
  const phoneKey = Buffer.isBuffer(phoneIdentityPublicKey)
    ? Buffer.from(phoneIdentityPublicKey)
    : decodeBase64url(phoneIdentityPublicKey, 32, 'invalid_public_key');
  if (phoneKey.length !== 32) throw new Error('invalid_public_key');
  return createHmac('sha256', key).update(lengthPrefixed([
    BOOTSTRAP_PROOF_DOMAIN,
    transcript,
    phoneKey,
  ])).digest();
}

function signTranscript(privateKey, transcript, clientAuth = false) {
  const input = clientAuth
    ? Buffer.concat([transcript, lengthPrefixed(['client-auth'])])
    : transcript;
  return base64url(sign(null, input, privateKey));
}

function verifyTranscript(publicKeyRaw, transcript, signature, clientAuth = false) {
  const input = clientAuth
    ? Buffer.concat([transcript, lengthPrefixed(['client-auth'])])
    : transcript;
  return verify(
    null,
    input,
    publicKeyFromRaw('ed25519', publicKeyRaw),
    decodeBase64url(signature, 64, 'invalid_signature'),
  );
}

function deriveDirectionalKeys(
  localPrivateKey,
  peerPublicKeyRaw,
  transcript,
  fields,
  bootstrapProofBytes = null,
) {
  const shared = diffieHellman({
    privateKey: localPrivateKey,
    publicKey: publicKeyFromRaw('x25519', peerPublicKeyRaw),
  });
  let binding;
  if (fields.handshake_mode === 'qr_bootstrap') {
    binding = Buffer.isBuffer(bootstrapProofBytes)
      ? Buffer.from(bootstrapProofBytes)
      : decodeBase64url(bootstrapProofBytes, 32, 'invalid_bootstrap_proof');
    if (binding.length !== 32) throw new Error('invalid_bootstrap_proof');
  } else {
    if (bootstrapProofBytes !== null) throw new Error('invalid_bootstrap_proof');
    binding = Buffer.from(fields.handshake_mode, 'utf8');
  }
  const salt = createHash('sha256').update(lengthPrefixed([
    KDF_BINDING_DOMAIN,
    transcript,
    binding,
  ])).digest();
  const prefix = `${HANDSHAKE_DOMAIN}|${fields.session_id}|${fields.mac_device_id}|${fields.phone_device_id}|${fields.key_epoch}`;
  return {
    phoneToMac: Buffer.from(hkdfSync('sha256', shared, salt, `${prefix}|phoneToMac`, 32)),
    macToPhone: Buffer.from(hkdfSync('sha256', shared, salt, `${prefix}|macToPhone`, 32)),
  };
}

function parseCounter(value) {
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
  const nonce = Buffer.alloc(12);
  nonce[0] = sender === 'mac' ? 1 : 2;
  nonce.writeBigUInt64BE(value, 4);
  return nonce;
}

function envelopeAad(fields) {
  return lengthPrefixed([
    ENVELOPE_DOMAIN,
    fields.session_id,
    String(fields.v),
    String(fields.key_epoch),
    fields.sender,
    fields.counter,
  ]);
}

function encryptEnvelope({ key, sessionId, sender, counter, plaintext }) {
  const bytes = Buffer.isBuffer(plaintext) ? plaintext : Buffer.from(plaintext, 'utf8');
  if (bytes.length === 0 || bytes.length > MAX_PLAINTEXT_BYTES) throw new Error('payload_too_large');
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
  const cipher = createCipheriv('aes-256-gcm', key, nonceFor(sender, counterText), {
    authTagLength: 16,
  });
  cipher.setAAD(envelopeAad(header));
  const ciphertext = Buffer.concat([cipher.update(bytes), cipher.final()]);
  return {
    ...header,
    ciphertext: base64url(ciphertext),
    tag: base64url(cipher.getAuthTag()),
  };
}

function decryptEnvelope({ key, envelope, sessionId, sender, expectedCounter }) {
  assertExactKeys(envelope, [
    'kind', 'v', 'session_id', 'key_epoch', 'sender', 'counter', 'ciphertext', 'tag',
  ], 'invalid_envelope');
  if (
    envelope.kind !== 'encrypted_envelope'
    || envelope.v !== PROTOCOL_VERSION
    || envelope.session_id !== sessionId
    || envelope.key_epoch !== KEY_EPOCH
    || envelope.sender !== sender
  ) throw new Error('invalid_envelope');
  const counter = parseCounter(envelope.counter);
  if (counter !== expectedCounter) throw new Error('replay_rejected');
  const ciphertext = decodeBase64url(envelope.ciphertext, Buffer.from(envelope.ciphertext, 'base64url').length, 'invalid_envelope');
  if (ciphertext.length === 0 || ciphertext.length > MAX_PLAINTEXT_BYTES) throw new Error('invalid_envelope');
  const decipher = createDecipheriv('aes-256-gcm', key, nonceFor(sender, counter), {
    authTagLength: 16,
  });
  decipher.setAAD(envelopeAad(envelope));
  decipher.setAuthTag(decodeBase64url(envelope.tag, 16, 'invalid_envelope'));
  const plaintext = Buffer.concat([decipher.update(ciphertext), decipher.final()]);
  if (plaintext.length === 0 || plaintext.length > MAX_PLAINTEXT_BYTES) throw new Error('invalid_envelope');
  return { plaintext, counter };
}

module.exports = {
  BOOTSTRAP_PROOF_DOMAIN,
  ENVELOPE_DOMAIN,
  HANDSHAKE_DOMAIN,
  KEY_EPOCH,
  MAX_PLAINTEXT_BYTES,
  PROTOCOL_VERSION,
  assertExactKeys,
  base64url,
  bootstrapProof,
  decodeBase64url,
  decryptEnvelope,
  deriveDirectionalKeys,
  encryptEnvelope,
  generateEphemeral,
  generateIdentity,
  handshakeTranscript,
  identityPrivateKey,
  lengthPrefixed,
  nonceFor,
  parseCounter,
  privateKeyFromSeed,
  rawPublicKey,
  signTranscript,
  verifyTranscript,
};
