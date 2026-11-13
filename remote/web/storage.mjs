// Stable-device storage pattern adapted from Remodex secure device state slices.
// Copyright 2025-2026 Emanuele Di Pietro. Licensed under Apache-2.0.
// Modified for fail-closed IndexedDB persistence of non-extractable WebCrypto keys.
import {
  base64url,
  concatBytes,
  decodeBase64url,
  generateEphemeral,
  generateIdentity,
  verifyStoredIdentity,
} from './protocol.mjs';

const DATABASE_NAME = 'memex-remote-v1';
const PROBE_DATABASE_NAME = 'memex-remote-capability-v1';
const STORE_NAME = 'state';
const WRITER_LOCK_NAME = 'memex-remote-browser-writer-v1';

function capabilityFailure(stage) {
  return new Error(`unsupported_${stage}`);
}

async function requireCapability(stage, operation) {
  try {
    return await operation();
  } catch {
    throw capabilityFailure(stage);
  }
}

function requestResult(request) {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error('storage_unavailable'));
  });
}

export function openDatabase(factory = globalThis.indexedDB, name = DATABASE_NAME) {
  if (!factory || typeof factory.open !== 'function') {
    return Promise.reject(new Error('unsupported_browser'));
  }
  return new Promise((resolve, reject) => {
    const request = factory.open(name, 1);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(STORE_NAME)) database.createObjectStore(STORE_NAME);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error('storage_unavailable'));
    request.onblocked = () => reject(new Error('storage_unavailable'));
  });
}

export function deleteDatabase(factory = globalThis.indexedDB, name = PROBE_DATABASE_NAME) {
  return new Promise((resolve) => {
    if (!factory || typeof factory.deleteDatabase !== 'function') {
      resolve();
      return;
    }
    const request = factory.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => resolve();
    request.onblocked = () => resolve();
  });
}

export function getState(database, key) {
  try {
    return requestResult(database.transaction(STORE_NAME, 'readonly').objectStore(STORE_NAME).get(key));
  } catch {
    return Promise.reject(new Error('storage_unavailable'));
  }
}

export function putState(database, key, value) {
  return new Promise((resolve, reject) => {
    try {
      const transaction = database.transaction(STORE_NAME, 'readwrite');
      transaction.objectStore(STORE_NAME).put(value, key);
      transaction.oncomplete = () => resolve();
      transaction.onerror = () => reject(new Error('storage_unavailable'));
      transaction.onabort = () => reject(new Error('storage_unavailable'));
    } catch {
      reject(new Error('storage_unavailable'));
    }
  });
}

function sameRecord(left, right) {
  const expected = right === null ? undefined : right;
  if (left === expected) return true;
  if (
    !left || !expected
    || typeof left !== 'object' || typeof expected !== 'object'
    || Array.isArray(left) || Array.isArray(expected)
  ) return false;
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(expected).sort();
  return leftKeys.length === rightKeys.length
    && leftKeys.every((key, index) => (
      key === rightKeys[index] && left[key] === expected[key]
    ));
}

function compareAndChangeOutbox(database, expected, next) {
  return new Promise((resolve, reject) => {
    let failure = new Error('storage_unavailable');
    let settled = false;
    const fail = () => {
      if (settled) return;
      settled = true;
      reject(failure);
    };
    try {
      const transaction = database.transaction(STORE_NAME, 'readwrite');
      const store = transaction.objectStore(STORE_NAME);
      transaction.oncomplete = () => {
        if (settled) return;
        settled = true;
        resolve();
      };
      transaction.onerror = fail;
      transaction.onabort = fail;
      const request = store.get('outbox');
      request.onerror = fail;
      request.onsuccess = () => {
        if (!sameRecord(request.result, expected)) {
          failure = new Error('outbox_conflict');
          transaction.abort();
          return;
        }
        try {
          store.put(next, 'outbox');
        } catch {
          transaction.abort();
        }
      };
    } catch {
      fail();
    }
  });
}

export function compareAndSetOutbox(database, expected, next) {
  return compareAndChangeOutbox(database, expected, next);
}

function hasOutboxIdentity(value, identity) {
  return Boolean(value && identity)
    && value.request_key === identity.request_key
    && value.url === identity.url
    && value.mac_device_id === identity.mac_device_id
    && value.mac_identity_public_key === identity.mac_identity_public_key;
}

export function deleteOutboxByIdentity(database, identity) {
  return new Promise((resolve, reject) => {
    let result = { deleted: false, current: null };
    let settled = false;
    const fail = () => {
      if (settled) return;
      settled = true;
      reject(new Error('storage_unavailable'));
    };
    try {
      const transaction = database.transaction(STORE_NAME, 'readwrite');
      const store = transaction.objectStore(STORE_NAME);
      transaction.oncomplete = () => {
        if (settled) return;
        settled = true;
        resolve(result);
      };
      transaction.onerror = fail;
      transaction.onabort = fail;
      const request = store.get('outbox');
      request.onerror = fail;
      request.onsuccess = () => {
        const current = request.result;
        result = { deleted: false, current: current ?? null };
        if (!hasOutboxIdentity(current, identity)) return;
        try {
          store.delete('outbox');
          result = { deleted: true, current };
        } catch {
          transaction.abort();
        }
      };
    } catch {
      fail();
    }
  });
}

export function acquireBrowserLease(locks = globalThis.navigator?.locks) {
  if (!locks || typeof locks.request !== 'function') {
    return Promise.reject(new Error('unsupported_browser'));
  }
  return new Promise((resolve, reject) => {
    let releaseLock;
    let requestCompletion;
    const hold = new Promise((release) => { releaseLock = release; });
    try {
      requestCompletion = Promise.resolve(locks.request(
        WRITER_LOCK_NAME,
        { mode: 'exclusive', ifAvailable: true },
        async (lock) => {
          if (!lock) {
            reject(new Error('another_tab_active'));
            return;
          }
          let released = false;
          resolve({
            async release() {
              if (released) return;
              released = true;
              releaseLock();
              await requestCompletion;
            },
          });
          await hold;
        },
      ));
      requestCompletion.catch(() => reject(new Error('storage_unavailable')));
    } catch {
      reject(new Error('unsupported_browser'));
    }
  });
}

export function deleteState(database, key) {
  return new Promise((resolve, reject) => {
    try {
      const transaction = database.transaction(STORE_NAME, 'readwrite');
      transaction.objectStore(STORE_NAME).delete(key);
      transaction.oncomplete = () => resolve();
      transaction.onerror = () => reject(new Error('storage_unavailable'));
      transaction.onabort = () => reject(new Error('storage_unavailable'));
    } catch {
      reject(new Error('storage_unavailable'));
    }
  });
}

async function assertNonExtractable(key, expectedAlgorithm, expectedUsage, subtle) {
  if (
    !key
    || key.type !== 'private'
    || key.extractable
    || key.algorithm?.name !== expectedAlgorithm
    || !key.usages.includes(expectedUsage)
  ) throw new Error('unsupported_browser');
  let exported = false;
  try {
    await subtle.exportKey('pkcs8', key);
    exported = true;
  } catch {
    // Rejection is required for a non-extractable private key.
  }
  if (exported) throw new Error('unsupported_browser');
}

export async function runCapabilityProbe({
  crypto = globalThis.crypto,
  indexedDB = globalThis.indexedDB,
  locks = globalThis.navigator?.locks,
  secureContext = globalThis.isSecureContext,
} = {}) {
  if (!secureContext) throw capabilityFailure('secure_context');
  if (!crypto?.subtle || typeof crypto.getRandomValues !== 'function') {
    throw capabilityFailure('webcrypto');
  }
  if (!locks || typeof locks.request !== 'function') throw capabilityFailure('web_locks');
  const subtle = crypto.subtle;
  await deleteDatabase(indexedDB, PROBE_DATABASE_NAME);
  let database;
  let reopened;
  try {
    const identity = await requireCapability('ed25519', async () => {
      const value = await generateIdentity(subtle);
      const challenge = crypto.getRandomValues(new Uint8Array(32));
      const publicKey = await subtle.importKey(
        'raw',
        decodeBase64url(value.publicKey, 32, 'unsupported_browser'),
        'Ed25519',
        true,
        ['verify'],
      );
      const signature = await subtle.sign('Ed25519', value.privateKey, challenge);
      if (!await subtle.verify('Ed25519', publicKey, signature, challenge)) {
        throw new Error('unsupported_browser');
      }
      await assertNonExtractable(value.privateKey, 'Ed25519', 'sign', subtle);
      return value;
    });

    const agreement = await requireCapability('x25519', async () => {
      const local = await generateEphemeral(subtle);
      const peer = await generateEphemeral(subtle);
      const peerPublic = await subtle.importKey('raw', peer.publicKeyRaw, 'X25519', true, []);
      const localPublic = await subtle.importKey('raw', local.publicKeyRaw, 'X25519', true, []);
      const localShared = new Uint8Array(await subtle.deriveBits(
        { name: 'X25519', public: peerPublic }, local.privateKey, 256,
      ));
      const peerShared = new Uint8Array(await subtle.deriveBits(
        { name: 'X25519', public: localPublic }, peer.privateKey, 256,
      ));
      if (
        localShared.length !== 32
        || localShared.some((value, index) => value !== peerShared[index])
      ) throw new Error('unsupported_browser');
      await assertNonExtractable(local.privateKey, 'X25519', 'deriveBits', subtle);
      return { local, localShared, peerPublic };
    });

    await requireCapability('hkdf_aes_gcm', async () => {
      const hkdf = await subtle.importKey('raw', agreement.localShared, 'HKDF', false, ['deriveBits']);
      const derived = new Uint8Array(await subtle.deriveBits({
        name: 'HKDF',
        hash: 'SHA-256',
        salt: new Uint8Array(32),
        info: new TextEncoder().encode('memex-browser-capability-v1'),
      }, hkdf, 256));
      const aes = await subtle.importKey('raw', derived, 'AES-GCM', false, ['encrypt', 'decrypt']);
      const iv = crypto.getRandomValues(new Uint8Array(12));
      const aad = new TextEncoder().encode('memex-browser-capability-aad-v1');
      const clear = new Uint8Array([1, 2, 3, 4]);
      const sealed = await subtle.encrypt(
        { name: 'AES-GCM', iv, additionalData: aad, tagLength: 128 }, aes, clear,
      );
      const opened = new Uint8Array(await subtle.decrypt(
        { name: 'AES-GCM', iv, additionalData: aad, tagLength: 128 }, aes, sealed,
      ));
      if (opened.some((value, index) => value !== clear[index])) {
        throw new Error('unsupported_browser');
      }
      let tamperAccepted = false;
      try {
        await subtle.decrypt({
          name: 'AES-GCM',
          iv,
          additionalData: concatBytes([aad, new Uint8Array([0])]),
          tagLength: 128,
        }, aes, sealed);
        tamperAccepted = true;
      } catch {
        // Authentication failure is required.
      }
      if (tamperAccepted) throw new Error('unsupported_browser');
    });

    await requireCapability('indexeddb_write', async () => {
      database = await openDatabase(indexedDB, PROBE_DATABASE_NAME);
      // Only the stable Ed25519 identity is durable. X25519 is connection-ephemeral,
      // and WebKit currently cannot deserialize an X25519 CryptoKeyOKP from IndexedDB.
      await putState(database, 'probe', {
        edPrivate: identity.privateKey,
        edPublic: identity.publicKey,
      });
    });
    database.close();
    database = null;
    const persisted = await requireCapability('indexeddb_reopen', async () => {
      reopened = await openDatabase(indexedDB, PROBE_DATABASE_NAME);
      const value = await getState(reopened, 'probe');
      if (!value) throw new Error('unsupported_browser');
      return value;
    });
    await requireCapability('indexeddb_cryptokey', async () => {
      await assertNonExtractable(persisted.edPrivate, 'Ed25519', 'sign', subtle);
      await verifyStoredIdentity({
        privateKey: persisted.edPrivate,
        publicKey: persisted.edPublic,
      }, subtle);
    });
    await requireCapability('indexeddb_write', () => deleteState(reopened, 'probe'));
  } finally {
    database?.close();
    reopened?.close();
    await deleteDatabase(indexedDB, PROBE_DATABASE_NAME);
  }
}

export async function loadOrCreateIdentity(database, subtle = globalThis.crypto.subtle) {
  const existing = await getState(database, 'identity');
  if (existing) return verifyStoredIdentity(existing, subtle);
  const identity = await generateIdentity(subtle);
  await putState(database, 'identity', identity);
  return verifyStoredIdentity(await getState(database, 'identity'), subtle);
}

export async function loadOrCreatePhoneId(database, crypto = globalThis.crypto) {
  const existing = await getState(database, 'phone_device_id');
  if (typeof existing === 'string' && /^web_[A-Za-z0-9_-]{22}$/.test(existing)) return existing;
  if (existing !== undefined) throw new Error('storage_unavailable');
  const value = `web_${base64url(crypto.getRandomValues(new Uint8Array(16)))}`;
  await putState(database, 'phone_device_id', value);
  return value;
}
