// Identity/trust-state pattern adapted from Remodex secure-device-state.js.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Modified for MemeX to keep private identity and bootstrap trust in Keychain,
// with a generation-CAS transaction protected by an owner-only process lock.
'use strict';

const { dataDir } = require('../data-dir');
const fs = require('node:fs');
const path = require('node:path');
const { randomBytes, randomUUID } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const {
  assertExactKeys,
  base64url,
  decodeBase64url,
  generateIdentity,
  identityPrivateKey,
} = require('../protocol');

const DEFAULT_KEYCHAIN_SERVICE = 'com.lotsmemex.remote';
const KEYCHAIN_SERVICE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]{0,127}$/;

/** Keychain item names; the legacy one is only ever read, to migrate a pairing. */
function keychainServices(env = process.env) {
  const current = env.MEMEX_REMOTE_KEYCHAIN_SERVICE || DEFAULT_KEYCHAIN_SERVICE;
  const legacy = env.MEMEX_REMOTE_KEYCHAIN_LEGACY_SERVICE || null;
  for (const name of [current, legacy]) {
    if (name !== null && !KEYCHAIN_SERVICE_PATTERN.test(name)) throw new Error('invalid_keychain_service');
  }
  return { current, legacy: legacy === current ? null : legacy };
}
const KEYCHAIN_SERVICE = DEFAULT_KEYCHAIN_SERVICE;
const KEYCHAIN_ACCOUNT = 'mac-bridge-state-v1';
const KEYCHAIN_HELPER = path.join(__dirname, 'keychain-helper.swift');
const DEFAULT_LOCK_PATH = path.join(dataDir(), 'remote-state.lock');
const DEFAULT_BRIDGE_LOCK_PATH = path.join(dataDir(), 'remote-bridge.lock');
const LOCK_TIMEOUT_MS = 3_000;

function runKeychainHelper(action, input, service = keychainServices().current) {
  return spawnSync('/usr/bin/xcrun', ['swift', KEYCHAIN_HELPER, action, service], {
    encoding: null,
    input,
    maxBuffer: 128 * 1024,
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 15_000,
  });
}

function readKeychainItem(service) {
  const result = runKeychainHelper('read', undefined, service);
  if (result.error) throw new Error('keychain_read_failed');
  if (result.status === 44) return null;
  if (result.status !== 0) throw new Error('keychain_read_failed');
  try {
    return JSON.parse(result.stdout.toString('utf8'));
  } catch {
    throw new Error('invalid_bridge_state');
  }
}

function readKeychainState() {
  const { current, legacy } = keychainServices();
  const state = readKeychainItem(current);
  if (state !== null || legacy === null) return state;
  // Copy an existing pairing forward once; the legacy item is left untouched.
  const migrated = readKeychainItem(legacy);
  if (migrated !== null) writeKeychainState(migrated);
  return migrated;
}

function writeKeychainState(state) {
  const serialized = Buffer.from(JSON.stringify(state), 'utf8');
  const result = runKeychainHelper('write', serialized);
  if (result.error || result.status !== 0) throw new Error('keychain_write_failed');
}

function validateTrustedPhone(phone) {
  if (phone === null) return;
  assertExactKeys(phone, ['phone_device_id', 'phone_identity_public_key'], 'invalid_bridge_state');
  if (
    typeof phone.phone_device_id !== 'string'
    || phone.phone_device_id.length === 0
    || phone.phone_device_id.length > 128
  ) throw new Error('invalid_bridge_state');
  decodeBase64url(phone.phone_identity_public_key, 32, 'invalid_bridge_state');
}

function validateStateV2(state) {
  assertExactKeys(state, [
    'version', 'generation', 'mac_device_id', 'mac_identity_public_key',
    'mac_identity_private_key', 'session_id', 'pairing_expires_at_ms',
    'bootstrap_secret', 'trusted_phone',
  ], 'invalid_bridge_state');
  if (
    state.version !== 2
    || !Number.isSafeInteger(state.generation)
    || state.generation < 1
    || typeof state.mac_device_id !== 'string'
    || !/^[0-9a-f-]{36}$/.test(state.mac_device_id)
    || !Number.isSafeInteger(state.pairing_expires_at_ms)
    || state.pairing_expires_at_ms < 0
  ) throw new Error('invalid_bridge_state');
  decodeBase64url(state.mac_identity_public_key, 32, 'invalid_bridge_state');
  identityPrivateKey(state.mac_identity_private_key);
  decodeBase64url(state.session_id, 32, 'invalid_bridge_state');
  if (state.bootstrap_secret !== null) {
    decodeBase64url(state.bootstrap_secret, 32, 'invalid_bridge_state');
    if (state.trusted_phone !== null || state.pairing_expires_at_ms === 0) {
      throw new Error('invalid_bridge_state');
    }
  } else if (state.pairing_expires_at_ms !== 0) {
    throw new Error('invalid_bridge_state');
  }
  validateTrustedPhone(state.trusted_phone);
  return state;
}

function validatePendingPairing(pending, state) {
  if (pending === null) return;
  assertExactKeys(pending, [
    'generation', 'session_id', 'pairing_expires_at_ms', 'bootstrap_secret',
  ], 'invalid_bridge_state');
  if (
    !Number.isSafeInteger(pending.generation)
    || pending.generation <= state.generation
    || pending.generation > state.pairing_counter
    || pending.session_id === state.session_id
    || !Number.isSafeInteger(pending.pairing_expires_at_ms)
    || pending.pairing_expires_at_ms < 1
  ) throw new Error('invalid_bridge_state');
  decodeBase64url(pending.session_id, 32, 'invalid_bridge_state');
  decodeBase64url(pending.bootstrap_secret, 32, 'invalid_bridge_state');
}

function validateState(state) {
  assertExactKeys(state, [
    'version', 'generation', 'pairing_counter', 'mac_device_id',
    'mac_identity_public_key', 'mac_identity_private_key', 'session_id',
    'trusted_phone', 'pending_pairing',
  ], 'invalid_bridge_state');
  if (
    state.version !== 3
    || !Number.isSafeInteger(state.generation)
    || state.generation < 1
    || !Number.isSafeInteger(state.pairing_counter)
    || state.pairing_counter < state.generation
    || typeof state.mac_device_id !== 'string'
    || !/^[0-9a-f-]{36}$/.test(state.mac_device_id)
  ) throw new Error('invalid_bridge_state');
  decodeBase64url(state.mac_identity_public_key, 32, 'invalid_bridge_state');
  identityPrivateKey(state.mac_identity_private_key);
  decodeBase64url(state.session_id, 32, 'invalid_bridge_state');
  validateTrustedPhone(state.trusted_phone);
  validatePendingPairing(state.pending_pairing, state);
  return state;
}

function decodeState(raw) {
  if (raw?.version === 1) {
    assertExactKeys(raw, [
      'version', 'mac_device_id', 'mac_identity_public_key', 'mac_identity_private_key',
      'session_id', 'pairing_expires_at_ms', 'trusted_phone',
    ], 'invalid_bridge_state');
    return {
      migrated: true,
      state: validateState({
        version: 3,
        generation: 2,
        pairing_counter: 2,
        mac_device_id: raw.mac_device_id,
        mac_identity_public_key: raw.mac_identity_public_key,
        mac_identity_private_key: raw.mac_identity_private_key,
        session_id: base64url(randomBytes(32)),
        trusted_phone: null,
        pending_pairing: null,
      }),
    };
  }
  if (raw?.version === 2) {
    const state = validateStateV2(raw);
    return {
      migrated: true,
      state: validateState({
        version: 3,
        generation: state.generation,
        pairing_counter: state.generation,
        mac_device_id: state.mac_device_id,
        mac_identity_public_key: state.mac_identity_public_key,
        mac_identity_private_key: state.mac_identity_private_key,
        session_id: state.session_id,
        trusted_phone: state.trusted_phone,
        // A v2 pending admission had already destroyed its predecessor trust.
        // Discard it fail-closed; a trusted v2 state is preserved exactly.
        pending_pairing: null,
      }),
    };
  }
  return { migrated: false, state: validateState(raw) };
}

function newState() {
  const identity = generateIdentity();
  return validateState({
    version: 3,
    generation: 1,
    pairing_counter: 1,
    mac_device_id: randomUUID(),
    mac_identity_public_key: identity.public_key,
    mac_identity_private_key: identity.private_key_pkcs8,
    session_id: base64url(randomBytes(32)),
    trusted_phone: null,
    pending_pairing: null,
  });
}

function secureLockParent(lockPath) {
  const parent = path.dirname(lockPath);
  fs.mkdirSync(parent, { recursive: true, mode: 0o700 });
  const metadata = fs.lstatSync(parent);
  if (
    metadata.isSymbolicLink()
    || !metadata.isDirectory()
    || metadata.uid !== process.geteuid()
    || (metadata.mode & 0o022) !== 0
    || fs.realpathSync(parent) !== path.resolve(parent)
  ) throw new Error('unsafe_state_lock');
}

function openLockFile(lockPath) {
  let descriptor;
  try {
    descriptor = fs.openSync(
      lockPath,
      fs.constants.O_RDWR | fs.constants.O_CREAT | fs.constants.O_NOFOLLOW,
      0o600,
    );
  } catch {
    throw new Error('unsafe_state_lock');
  }
  let metadata;
  let pathMetadata;
  try {
    metadata = fs.fstatSync(descriptor);
    pathMetadata = fs.lstatSync(lockPath);
  } catch {
    fs.closeSync(descriptor);
    throw new Error('unsafe_state_lock');
  }
  if (
    !metadata.isFile()
    || metadata.uid !== process.geteuid()
    || metadata.nlink !== 1
    || (metadata.mode & 0o177) !== 0
    || pathMetadata.dev !== metadata.dev
    || pathMetadata.ino !== metadata.ino
    || pathMetadata.isSymbolicLink()
  ) {
    fs.closeSync(descriptor);
    throw new Error('unsafe_state_lock');
  }
  return descriptor;
}

function withFileLock(
  lockPath,
  operation,
  { timeoutMs = LOCK_TIMEOUT_MS } = {},
) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 0 || timeoutMs > 60_000) {
    throw new Error('state_lock_failed');
  }
  secureLockParent(lockPath);
  const descriptor = openLockFile(lockPath);
  try {
    const timeoutSeconds = timeoutMs === 0 ? 0 : Math.ceil(timeoutMs / 1_000);
    const lock = spawnSync('/usr/bin/lockf', ['-s', '-t', String(timeoutSeconds), '3'], {
      encoding: null,
      stdio: ['ignore', 'ignore', 'ignore', descriptor],
      timeout: timeoutMs + 1_000,
    });
    if (lock.status === 75) throw new Error('state_lock_busy');
    if (lock.error || lock.status !== 0) throw new Error('state_lock_failed');
    return operation();
  } finally {
    fs.closeSync(descriptor);
  }
}

function acquireProcessLease(lockPath = DEFAULT_BRIDGE_LOCK_PATH) {
  secureLockParent(lockPath);
  const descriptor = openLockFile(lockPath);
  const lock = spawnSync('/usr/bin/lockf', ['-s', '-t', '0', '3'], {
    encoding: null,
    stdio: ['ignore', 'ignore', 'ignore', descriptor],
    timeout: 1_000,
  });
  if (lock.status === 75) {
    fs.closeSync(descriptor);
    throw new Error('bridge_already_running');
  }
  if (lock.error || lock.status !== 0) {
    fs.closeSync(descriptor);
    throw new Error('bridge_lock_failed');
  }
  let released = false;
  return {
    release() {
      if (released) return;
      released = true;
      fs.closeSync(descriptor);
    },
  };
}

class MacKeychainStateStore {
  constructor({
    readState = readKeychainState,
    writeState = writeKeychainState,
    lockPath = DEFAULT_LOCK_PATH,
    lockOptions = {},
  } = {}) {
    this.readState = readState;
    this.writeState = writeState;
    this.lockPath = lockPath;
    this.lockOptions = lockOptions;
  }

  locked(operation) {
    return withFileLock(this.lockPath, operation, this.lockOptions);
  }

  loadUnlocked() {
    const raw = this.readState();
    if (!raw) return null;
    return decodeState(raw);
  }

  load() {
    return this.locked(() => {
      const decoded = this.loadUnlocked();
      if (!decoded) return null;
      if (decoded.migrated) this.writeState(decoded.state);
      return structuredClone(decoded.state);
    });
  }

  loadOrCreate() {
    return this.locked(() => {
      const decoded = this.loadUnlocked();
      if (decoded) {
        if (decoded.migrated) this.writeState(decoded.state);
        return structuredClone(decoded.state);
      }
      const state = newState();
      this.writeState(state);
      return structuredClone(state);
    });
  }

  update(expectedGeneration, mutation) {
    return this.locked(() => {
      const decoded = this.loadUnlocked();
      const current = decoded?.state || newState();
      if (expectedGeneration !== null && current.generation !== expectedGeneration) {
        throw new Error('stale_generation');
      }
      const next = validateState(mutation(structuredClone(current)));
      if (next.generation < current.generation
        || next.pairing_counter < current.pairing_counter) throw new Error('stale_generation');
      this.writeState(next);
      return structuredClone(next);
    });
  }

  withTrustedChannel(expected, operation) {
    return this.locked(() => withTrustedState(this.loadUnlocked()?.state, expected, operation));
  }
}

class MemoryStateStore {
  constructor(state = null) { this.state = state ? structuredClone(state) : null; }
  load() { return this.state ? validateState(structuredClone(this.state)) : null; }
  loadOrCreate() {
    if (!this.state) this.state = newState();
    return this.load();
  }
  update(expectedGeneration, mutation) {
    const current = this.loadOrCreate();
    if (expectedGeneration !== null && current.generation !== expectedGeneration) {
      throw new Error('stale_generation');
    }
    const next = validateState(mutation(current));
    if (next.generation < current.generation
      || next.pairing_counter < current.pairing_counter) throw new Error('stale_generation');
    this.state = structuredClone(next);
    return this.load();
  }
  withTrustedChannel(expected, operation) {
    return withTrustedState(this.load(), expected, operation);
  }
}

function withTrustedState(state, expected, operation) {
  if (
    !state
    || state.generation !== expected.generation
    || state.session_id !== expected.sessionId
    || !state.trusted_phone
    || state.trusted_phone.phone_device_id !== expected.phoneDeviceId
    || state.trusted_phone.phone_identity_public_key !== expected.phoneIdentityPublicKey
  ) throw new Error('untrusted_phone');
  const result = operation();
  if (result && typeof result.then === 'function') throw new Error('async_state_lock_operation');
  return result;
}

function beginFreshPairing(store, now = Date.now(), durationMs = 5 * 60_000) {
  if (!Number.isSafeInteger(now) || now < 0) throw new Error('invalid_pairing_time');
  if (![5 * 60_000, 30 * 60_000].includes(durationMs)
    || !Number.isSafeInteger(now + durationMs)) throw new Error('invalid_pairing_duration');
  const state = store.update(null, (current) => {
    current.pairing_counter += 1;
    current.pending_pairing = {
      generation: current.pairing_counter,
      session_id: base64url(randomBytes(32)),
      pairing_expires_at_ms: now + durationMs,
      bootstrap_secret: base64url(randomBytes(32)),
    };
    return current;
  });
  return pendingPairingState(state);
}

function pendingPairingState(state) {
  const pending = state.pending_pairing;
  if (!pending) throw new Error('pairing_not_active');
  return {
    version: state.version,
    generation: pending.generation,
    mac_device_id: state.mac_device_id,
    mac_identity_public_key: state.mac_identity_public_key,
    mac_identity_private_key: state.mac_identity_private_key,
    session_id: pending.session_id,
    pairing_expires_at_ms: pending.pairing_expires_at_ms,
    bootstrap_secret: pending.bootstrap_secret,
    trusted_phone: null,
  };
}

function cancelPendingPairing(store, expectedGeneration) {
  return store.update(null, (state) => {
    if (!state.pending_pairing
      || state.pending_pairing.generation !== expectedGeneration) {
      throw new Error('stale_generation');
    }
    state.pending_pairing = null;
    return state;
  });
}

function revokeTrust(store) {
  return store.update(null, (state) => {
    state.pairing_counter += 1;
    state.generation = state.pairing_counter;
    state.session_id = base64url(randomBytes(32));
    state.trusted_phone = null;
    state.pending_pairing = null;
    return state;
  });
}

function expirePairing(store, expectedGeneration, now = Date.now()) {
  return store.update(null, (state) => {
    const pending = state.pending_pairing;
    if (
      !pending
      || pending.generation !== expectedGeneration
      || pending.pairing_expires_at_ms > now
    ) throw new Error('pairing_not_expired');
    state.pending_pairing = null;
    return state;
  });
}

function commitPhoneTrust(store, expected) {
  return store.update(null, (state) => {
    const pending = state.pending_pairing;
    if (
      !pending
      || pending.generation !== expected.generation
      || pending.session_id !== expected.sessionId
      || pending.pairing_expires_at_ms <= expected.now
    ) throw new Error('stale_generation');
    state.generation = pending.generation;
    state.session_id = pending.session_id;
    state.trusted_phone = {
      phone_device_id: expected.phoneDeviceId,
      phone_identity_public_key: expected.phoneIdentityPublicKey,
    };
    state.pending_pairing = null;
    return state;
  });
}

module.exports = {
  DEFAULT_BRIDGE_LOCK_PATH,
  DEFAULT_LOCK_PATH,
  KEYCHAIN_ACCOUNT,
  KEYCHAIN_SERVICE,
  keychainServices,
  MacKeychainStateStore,
  MemoryStateStore,
  acquireProcessLease,
  beginFreshPairing,
  cancelPendingPairing,
  commitPhoneTrust,
  expirePairing,
  newState,
  pendingPairingState,
  revokeTrust,
  validateState,
  withFileLock,
};
