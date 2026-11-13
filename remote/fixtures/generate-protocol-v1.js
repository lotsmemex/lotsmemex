'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { deriveCapability } = require('../admission');
const {
  PROTOCOL_VERSION,
  base64url,
  bootstrapProof,
  deriveDirectionalKeys,
  encryptEnvelope,
  handshakeTranscript,
  privateKeyFromSeed,
  rawPublicKey,
  signTranscript,
} = require('../protocol');

function bytes(start) {
  return Buffer.from(Array.from({ length: 32 }, (_, index) => (start + index) & 0xff));
}

function buildFixture() {
  const macIdentitySeed = bytes(0x00);
  const phoneIdentitySeed = bytes(0x20);
  const macEphemeralSeed = bytes(0x40);
  const phoneEphemeralSeed = bytes(0x60);
  const bootstrapSecret = bytes(0xe0);
  const admissionRoot = bytes(0x10);
  const macIdentity = privateKeyFromSeed('ed25519', macIdentitySeed);
  const phoneIdentity = privateKeyFromSeed('ed25519', phoneIdentitySeed);
  const macEphemeral = privateKeyFromSeed('x25519', macEphemeralSeed);
  const phoneEphemeral = privateKeyFromSeed('x25519', phoneEphemeralSeed);
  const fields = {
    protocol_version: PROTOCOL_VERSION,
    session_id: base64url(bytes(0xa0)),
    pairing_generation: 7,
    handshake_mode: 'qr_bootstrap',
    key_epoch: 1,
    mac_device_id: '11111111-1111-4111-8111-111111111111',
    phone_device_id: '22222222-2222-4222-8222-222222222222',
    mac_identity_public_key: base64url(rawPublicKey(macIdentity)),
    phone_identity_public_key: base64url(rawPublicKey(phoneIdentity)),
    mac_ephemeral_public_key: base64url(rawPublicKey(macEphemeral)),
    phone_ephemeral_public_key: base64url(rawPublicKey(phoneEphemeral)),
    client_nonce: base64url(bytes(0x80)),
    server_nonce: base64url(bytes(0xc0)),
    expires_at_ms: 1_800_000_000_000,
  };
  const transcript = handshakeTranscript(fields);
  const proof = bootstrapProof(
    bootstrapSecret,
    transcript,
    fields.phone_identity_public_key,
  );
  const macKeys = deriveDirectionalKeys(
    macEphemeral,
    rawPublicKey(phoneEphemeral),
    transcript,
    fields,
    proof,
  );
  const phoneKeys = deriveDirectionalKeys(
    phoneEphemeral,
    rawPublicKey(macEphemeral),
    transcript,
    fields,
    proof,
  );
  if (!macKeys.phoneToMac.equals(phoneKeys.phoneToMac) || !macKeys.macToPhone.equals(phoneKeys.macToPhone)) {
    throw new Error('fixture_key_mismatch');
  }
  const rpc = {
    id: 'rpc_fixture_1',
    method: 'memex.capture.status',
    params: { job_id: 'job_00000000000000000000000000000000' },
  };
  return {
    note: 'Public deterministic test fixture; all keys and values are synthetic.',
    seeds: {
      mac_identity: base64url(macIdentitySeed),
      phone_identity: base64url(phoneIdentitySeed),
      mac_ephemeral: base64url(macEphemeralSeed),
      phone_ephemeral: base64url(phoneEphemeralSeed),
      bootstrap_secret: base64url(bootstrapSecret),
      admission_root: base64url(admissionRoot),
    },
    fields,
    transcript: base64url(transcript),
    mac_signature: signTranscript(macIdentity, transcript),
    phone_signature: signTranscript(phoneIdentity, transcript, true),
    bootstrap_proof: base64url(proof),
    mac_admission_capability: deriveCapability(admissionRoot, fields.session_id, 'mac', 'bridge'),
    iphone_admission_capability: deriveCapability(
      admissionRoot, fields.session_id, 'iphone', 'browser',
    ),
    native_iphone_admission_capability: deriveCapability(
      admissionRoot, fields.session_id, 'iphone', 'native',
    ),
    phone_to_mac_key: base64url(macKeys.phoneToMac),
    mac_to_phone_key: base64url(macKeys.macToPhone),
    rpc,
    envelope: encryptEnvelope({
      key: macKeys.phoneToMac,
      sessionId: fields.session_id,
      sender: 'iphone',
      counter: 0n,
      plaintext: JSON.stringify(rpc),
    }),
  };
}

if (require.main === module) {
  const destination = path.join(__dirname, 'protocol-v1.json');
  fs.writeFileSync(destination, `${JSON.stringify(buildFixture(), null, 2)}\n`, { mode: 0o644 });
}

module.exports = { buildFixture };
