'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { buildFixture } = require('../fixtures/generate-protocol-v1');
const { deriveCapability } = require('../admission');
const {
  base64url,
  bootstrapProof,
  decryptEnvelope,
  handshakeTranscript,
  privateKeyFromSeed,
  verifyTranscript,
} = require('../protocol');

const fixturePath = path.join(__dirname, '../fixtures/protocol-v1.json');

test('checked-in fixture exactly matches the deterministic protocol implementation', () => {
  const fixture = JSON.parse(fs.readFileSync(fixturePath, 'utf8'));
  assert.deepEqual(fixture, buildFixture());
  const transcript = handshakeTranscript(fixture.fields);
  assert.equal(base64url(bootstrapProof(
    fixture.seeds.bootstrap_secret,
    transcript,
    fixture.fields.phone_identity_public_key,
  )), fixture.bootstrap_proof);
  assert.equal(
    deriveCapability(fixture.seeds.admission_root, fixture.fields.session_id, 'mac', 'bridge'),
    fixture.mac_admission_capability,
  );
  assert.equal(
    deriveCapability(fixture.seeds.admission_root, fixture.fields.session_id, 'iphone', 'browser'),
    fixture.iphone_admission_capability,
  );
  assert.equal(
    deriveCapability(fixture.seeds.admission_root, fixture.fields.session_id, 'iphone', 'native'),
    fixture.native_iphone_admission_capability,
  );
  assert.equal(
    verifyTranscript(Buffer.from(fixture.fields.mac_identity_public_key, 'base64url'), transcript, fixture.mac_signature),
    true,
  );
  assert.equal(
    verifyTranscript(Buffer.from(fixture.fields.phone_identity_public_key, 'base64url'), transcript, fixture.phone_signature, true),
    true,
  );
  const decrypted = decryptEnvelope({
    key: Buffer.from(fixture.phone_to_mac_key, 'base64url'),
    envelope: fixture.envelope,
    sessionId: fixture.fields.session_id,
    sender: 'iphone',
    expectedCounter: 0n,
  });
  assert.deepEqual(JSON.parse(decrypted.plaintext), fixture.rpc);
});

test('AAD metadata tampering and replay both fail closed', () => {
  const fixture = buildFixture();
  const tampered = { ...fixture.envelope, sender: 'mac' };
  assert.throws(() => decryptEnvelope({
    key: Buffer.from(fixture.phone_to_mac_key, 'base64url'),
    envelope: tampered,
    sessionId: fixture.fields.session_id,
    sender: 'mac',
    expectedCounter: 0n,
  }));
  assert.throws(() => decryptEnvelope({
    key: Buffer.from(fixture.phone_to_mac_key, 'base64url'),
    envelope: fixture.envelope,
    sessionId: fixture.fields.session_id,
    sender: 'iphone',
    expectedCounter: 1n,
  }), /replay_rejected/);
});

test('synthetic seeded private key construction stays deterministic', () => {
  const fixture = buildFixture();
  assert.equal(
    privateKeyFromSeed('ed25519', Buffer.from(fixture.seeds.mac_identity, 'base64url')).type,
    'private',
  );
});
