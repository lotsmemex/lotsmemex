'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { once } = require('node:events');
const { spawn, spawnSync } = require('node:child_process');
const test = require('node:test');
const {
  MacKeychainStateStore,
  MemoryStateStore,
  acquireProcessLease,
  beginFreshPairing,
  cancelPendingPairing,
  commitPhoneTrust,
  expirePairing,
  revokeTrust,
  withFileLock,
} = require('../mac/state-store');
const { MacSecureSession } = require('../mac/secure-session');
const { base64url, generateEphemeral, generateIdentity } = require('../protocol');

function processEquivalentStore(root) {
  const statePath = path.join(root, 'synthetic-keychain.json');
  return new MacKeychainStateStore({
    lockPath: path.join(root, 'state.lock'),
    readState: () => {
      try { return JSON.parse(fs.readFileSync(statePath, 'utf8')); }
      catch (error) { if (error.code === 'ENOENT') return null; throw error; }
    },
    writeState: (state) => fs.writeFileSync(statePath, JSON.stringify(state), { mode: 0o600 }),
  });
}

test('gateway thirty-minute request revokes the old code and is consumed only once', (t) => {
  const { fileStateStore, requestFreshPairing, takePairingDuration } = require('../service/mobile-gateway');
  const dir = fs.mkdtempSync('/private/tmp/memex-pair-duration-');
  fs.chmodSync(dir, 0o700);
  t.after(() => fs.rmSync(dir, {recursive:true, force:true}));
  const store = fileStateStore(dir);
  const old = beginFreshPairing(store);
  requestFreshPairing(dir, 30);
  assert.equal(store.load().pending_pairing, null);
  assert.equal(store.load().trusted_phone, null);
  assert.notEqual(store.load().session_id, old.session_id);
  assert.equal(takePairingDuration(dir), 1800000);
  assert.equal(takePairingDuration(dir), 300000);
  const unchanged = store.load();
  assert.throws(() => requestFreshPairing(dir, 31), /invalid_pairing_duration/);
  assert.deepEqual(store.load(), unchanged);
  requestFreshPairing(dir);
  assert.equal(takePairingDuration(dir), 300000);
});

test('the five-minute bootstrap is unusable at its exact expiry boundary', () => {
  const store = new MemoryStateStore();
  const active = beginFreshPairing(store, 1_000);
  const phone = generateIdentity();
  assert.throws(() => commitPhoneTrust(store, {
    generation: active.generation,
    sessionId: active.session_id,
    phoneDeviceId: '55555555-5555-4555-8555-555555555555',
    phoneIdentityPublicKey: phone.public_key,
    now: active.pairing_expires_at_ms,
  }), /stale_generation/);
  const state = store.load();
  assert.equal(state.trusted_phone, null);
  assert.equal(state.pending_pairing.bootstrap_secret, active.bootstrap_secret);
});

test('v1 migration preserves Mac identity but rejects old phone trust and routing session', async () => {
  const root = fs.mkdtempSync(path.join('/private/tmp', 'memex-state-migration-'));
  const statePath = path.join(root, 'synthetic-keychain.json');
  const mac = generateIdentity();
  const phone = generateIdentity();
  const oldSession = Buffer.alloc(32, 0xa1).toString('base64url');
  fs.writeFileSync(statePath, JSON.stringify({
    version: 1,
    mac_device_id: '77777777-7777-4777-8777-777777777777',
    mac_identity_public_key: mac.public_key,
    mac_identity_private_key: mac.private_key_pkcs8,
    session_id: oldSession,
    pairing_expires_at_ms: 9_999_999,
    trusted_phone: {
      phone_device_id: '88888888-8888-4888-8888-888888888888',
      phone_identity_public_key: phone.public_key,
    },
  }), { mode: 0o600 });

  const store = processEquivalentStore(root);
  const migrated = store.load();
  assert.equal(migrated.version, 3);
  assert.equal(migrated.generation, 2);
  assert.equal(migrated.pairing_counter, 2);
  assert.equal(migrated.mac_identity_public_key, mac.public_key);
  assert.equal(migrated.mac_identity_private_key, mac.private_key_pkcs8);
  assert.notEqual(migrated.session_id, oldSession);
  assert.equal(migrated.pending_pairing, null);
  assert.equal(migrated.trusted_phone, null);
  assert.deepEqual(JSON.parse(fs.readFileSync(statePath, 'utf8')), migrated);

  const oldPhone = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: () => assert.fail('server hello must not be sent'),
  });
  const ephemeral = generateEphemeral();
  await assert.rejects(oldPhone.receive(JSON.stringify({
    kind: 'client_hello',
    protocol_version: 1,
    session_id: migrated.session_id,
    pairing_generation: migrated.generation,
    handshake_mode: 'trusted_reconnect',
    phone_device_id: '88888888-8888-4888-8888-888888888888',
    phone_identity_public_key: phone.public_key,
    phone_ephemeral_public_key: base64url(ephemeral.publicKey),
    client_nonce: Buffer.alloc(32, 0xb2).toString('base64url'),
  })), /untrusted_phone/);
  fs.rmSync(root, { recursive: true, force: true });
});

test('separate stores serialize pending admission replacement so stale trust cannot commit', () => {
  const root = fs.mkdtempSync(path.join('/private/tmp', 'memex-state-cas-'));
  const oldProcess = processEquivalentStore(root);
  const freshPairProcess = processEquivalentStore(root);
  const old = beginFreshPairing(oldProcess, 1_000);
  const phone = generateIdentity();
  const fresh = beginFreshPairing(freshPairProcess, 2_000);
  assert.throws(() => commitPhoneTrust(oldProcess, {
    generation: old.generation,
    sessionId: old.session_id,
    phoneDeviceId: '66666666-6666-4666-8666-666666666666',
    phoneIdentityPublicKey: phone.public_key,
    now: 1_500,
  }), /stale_generation/);
  const survived = freshPairProcess.load();
  assert.equal(survived.generation, 1);
  assert.equal(survived.pairing_counter, fresh.generation);
  assert.equal(survived.pending_pairing.generation, fresh.generation);
  assert.equal(survived.pending_pairing.session_id, fresh.session_id);
  assert.equal(survived.pending_pairing.bootstrap_secret, fresh.bootstrap_secret);
  assert.equal(survived.trusted_phone, null);
  fs.rmSync(root, { recursive: true, force: true });
});

test('v2 trusted migration preserves the exact live identity, route, generation, and phone', () => {
  const root = fs.mkdtempSync(path.join('/private/tmp', 'memex-state-v2-migration-'));
  const statePath = path.join(root, 'synthetic-keychain.json');
  const mac = generateIdentity();
  const phone = generateIdentity();
  const session = Buffer.alloc(32, 0xa2).toString('base64url');
  const trustedPhone = {
    phone_device_id: '99999999-9999-4999-8999-999999999999',
    phone_identity_public_key: phone.public_key,
  };
  fs.writeFileSync(statePath, JSON.stringify({
    version: 2,
    generation: 17,
    mac_device_id: '77777777-7777-4777-8777-777777777777',
    mac_identity_public_key: mac.public_key,
    mac_identity_private_key: mac.private_key_pkcs8,
    session_id: session,
    pairing_expires_at_ms: 0,
    bootstrap_secret: null,
    trusted_phone: trustedPhone,
  }), { mode: 0o600 });

  const migrated = processEquivalentStore(root).load();
  assert.equal(migrated.version, 3);
  assert.equal(migrated.generation, 17);
  assert.equal(migrated.pairing_counter, 17);
  assert.equal(migrated.session_id, session);
  assert.equal(migrated.mac_identity_public_key, mac.public_key);
  assert.deepEqual(migrated.trusted_phone, trustedPhone);
  assert.equal(migrated.pending_pairing, null);
  fs.rmSync(root, { recursive: true, force: true });
});

test('pending create, cancel, and expiry preserve trust; only explicit revoke destroys it', () => {
  const store = new MemoryStateStore();
  const first = beginFreshPairing(store, 1_000);
  const phone = generateIdentity();
  commitPhoneTrust(store, {
    generation: first.generation,
    sessionId: first.session_id,
    phoneDeviceId: '55555555-5555-4555-8555-555555555555',
    phoneIdentityPublicKey: phone.public_key,
    now: 2_000,
  });
  const trusted = store.load();
  const expected = {
    generation: trusted.generation,
    sessionId: trusted.session_id,
    phoneDeviceId: trusted.trusted_phone.phone_device_id,
    phoneIdentityPublicKey: trusted.trusted_phone.phone_identity_public_key,
  };

  const cancelled = beginFreshPairing(store, 3_000);
  assert.equal(store.withTrustedChannel(expected, () => 'active'), 'active');
  cancelPendingPairing(store, cancelled.generation);
  assert.equal(store.withTrustedChannel(expected, () => 'active'), 'active');

  const expiring = beginFreshPairing(store, 4_000);
  expirePairing(store, expiring.generation, expiring.pairing_expires_at_ms);
  assert.equal(store.withTrustedChannel(expected, () => 'active'), 'active');

  const beforeRevoke = store.load();
  const revoked = revokeTrust(store);
  assert.equal(revoked.trusted_phone, null);
  assert.equal(revoked.pending_pairing, null);
  assert.notEqual(revoked.session_id, beforeRevoke.session_id);
  assert.throws(() => store.withTrustedChannel(expected, () => {}), /untrusted_phone/);
});

test('kernel lock excludes a live child and releases automatically after its crash', async (t) => {
  const root = fs.mkdtempSync(path.join('/private/tmp', 'memex-state-lock-'));
  const lockPath = path.join(root, 'state.lock');
  const readyPath = path.join(root, 'ready');
  const modulePath = path.join(__dirname, '../mac/state-store.js');
  const child = spawn(process.execPath, ['-e', `
    const fs = require('node:fs');
    const { withFileLock } = require(${JSON.stringify(modulePath)});
    withFileLock(${JSON.stringify(lockPath)}, () => {
      fs.writeFileSync(${JSON.stringify(readyPath)}, 'ready');
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 60_000);
    });
  `], { stdio: 'ignore' });
  t.after(() => {
    if (child.exitCode === null) child.kill('SIGKILL');
    fs.rmSync(root, { recursive: true, force: true });
  });
  for (let attempt = 0; attempt < 500 && !fs.existsSync(readyPath); attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(fs.existsSync(readyPath), true);
  assert.throws(() => withFileLock(lockPath, () => {}, { timeoutMs: 0 }), /state_lock_busy/);
  assert.equal(fs.existsSync(lockPath), true);

  child.kill('SIGKILL');
  await once(child, 'exit');
  let ran = false;
  withFileLock(lockPath, () => { ran = true; }, { timeoutMs: 0 });
  assert.equal(ran, true);
  assert.equal(fs.existsSync(lockPath), true);
});

test('bridge singleton excludes a live process before pairing work and recovers after SIGKILL', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join('/private/tmp', 'memex-bridge-lease-'));
  const root = fs.realpathSync(rawRoot);
  const lockPath = path.join(root, 'bridge.lock');
  const readyPath = path.join(root, 'ready');
  const rotatedPath = path.join(root, 'pairing-rotated');
  const modulePath = path.join(__dirname, '../mac/state-store.js');
  const owner = spawn(process.execPath, ['-e', `
    const fs = require('node:fs');
    const { acquireProcessLease } = require(${JSON.stringify(modulePath)});
    acquireProcessLease(${JSON.stringify(lockPath)});
    fs.writeFileSync(${JSON.stringify(readyPath)}, 'ready');
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 60_000);
  `], { stdio: 'ignore' });
  t.after(() => {
    if (owner.exitCode === null) owner.kill('SIGKILL');
    fs.rmSync(root, { recursive: true, force: true });
  });
  for (let attempt = 0; attempt < 500 && !fs.existsSync(readyPath); attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.equal(fs.existsSync(readyPath), true);

  const contenderSource = `
    const fs = require('node:fs');
    const { acquireProcessLease } = require(${JSON.stringify(modulePath)});
    try {
      const lease = acquireProcessLease(${JSON.stringify(lockPath)});
      fs.writeFileSync(${JSON.stringify(rotatedPath)}, 'rotated');
      lease.release();
      process.stdout.write('acquired');
    } catch (error) { process.stdout.write(error.message); }
  `;
  const blocked = spawnSync(process.execPath, ['-e', contenderSource], { encoding: 'utf8' });
  assert.equal(blocked.stdout, 'bridge_already_running');
  assert.equal(fs.existsSync(rotatedPath), false);

  owner.kill('SIGKILL');
  await once(owner, 'exit');
  const recovered = spawnSync(process.execPath, ['-e', contenderSource], { encoding: 'utf8' });
  assert.equal(recovered.stdout, 'acquired');
  assert.equal(fs.readFileSync(rotatedPath, 'utf8'), 'rotated');
});

test('persistent lock path rejects symlinks and hard links', () => {
  const root = fs.mkdtempSync(path.join('/private/tmp', 'memex-state-lock-path-'));
  const lockPath = path.join(root, 'state.lock');
  const target = path.join(root, 'target');
  fs.writeFileSync(target, 'not-a-lock', { mode: 0o600 });
  fs.symlinkSync(target, lockPath);
  assert.throws(() => withFileLock(lockPath, () => {}), /unsafe_state_lock/);
  fs.unlinkSync(lockPath);
  fs.linkSync(target, lockPath);
  assert.throws(() => withFileLock(lockPath, () => {}), /unsafe_state_lock/);
  fs.rmSync(root, { recursive: true, force: true });
});

test('data directory defaults to LotsMemeX and accepts only a plain folder name', () => {
  const { dataDir } = require('../data-dir');
  const os = require('node:os');
  const path = require('node:path');
  const base = path.join(os.homedir(), 'Library/Application Support');
  assert.equal(dataDir({}), path.join(base, 'LotsMemeX'));
  assert.equal(dataDir({ MEMEX_DATA_DIR_NAME: 'MemeX' }), path.join(base, 'MemeX'));
  for (const name of ['../x', 'a/b', '.hidden', 'a..b', 'x'.repeat(65)]) {
    assert.throws(() => dataDir({ MEMEX_DATA_DIR_NAME: name }), /invalid_data_dir_name/);
  }
});
