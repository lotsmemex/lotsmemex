'use strict';

const assert = require('node:assert/strict');
const { webcrypto } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const {
  browserPairingCode, diagnosticWebClientUrl, nativePairingCode,
  parseWebClientUrl,
} = require('../mac/bridge');

const fixture = JSON.parse(fs.readFileSync(
  path.join(__dirname, '../fixtures/protocol-v1.json'),
  'utf8',
));

function privatePkcs8(seed, algorithmByte) {
  return Buffer.concat([
    Buffer.from(`302e020100300506032b65${algorithmByte}04220420`, 'hex'),
    Buffer.from(seed, 'base64url'),
  ]);
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

class MemoryDatabase {
  constructor(initial = {}) {
    this.state = new Map(Object.entries(initial).map(([key, value]) => [key, clone(value)]));
    this.abortNextWrite = false;
    this.nextCommitGate = null;
    this.transactionTail = Promise.resolve();
  }

  gateNextCommit() {
    let enter;
    let release;
    const entered = new Promise((resolve) => { enter = resolve; });
    const released = new Promise((resolve) => { release = resolve; });
    this.nextCommitGate = { enter, released };
    return { entered, release };
  }

  transaction(_storeName, mode) {
    const database = this;
    const commitGate = mode === 'readwrite' ? this.nextCommitGate : null;
    if (commitGate) this.nextCommitGate = null;
    const prior = this.transactionTail;
    let releaseTurn;
    const turnComplete = new Promise((resolve) => { releaseTurn = resolve; });
    this.transactionTail = prior.then(() => turnComplete);
    const operations = [];
    let aborted = false;
    const transaction = {
      oncomplete: null,
      onerror: null,
      onabort: null,
      abort() {
        if (aborted) return;
        aborted = true;
      },
      objectStore() {
        return {
          get(key) {
            const request = { result: undefined, onsuccess: null, onerror: null };
            operations.push({ type: 'get', key, request });
            return request;
          },
          put(value, key) {
            if (mode !== 'readwrite') throw new Error('readonly');
            operations.push({ type: 'put', key, value: clone(value) });
          },
          delete(key) {
            if (mode !== 'readwrite') throw new Error('readonly');
            operations.push({ type: 'delete', key });
          },
        };
      },
    };
    queueMicrotask(async () => {
      await prior;
      const staged = new Map([...database.state].map(([key, value]) => [key, clone(value)]));
      try {
        for (let index = 0; index < operations.length && !aborted; index += 1) {
          const operation = operations[index];
          if (operation.type === 'get') {
            operation.request.result = clone(staged.get(operation.key));
            operation.request.onsuccess?.();
            continue;
          }
          if (database.abortNextWrite) {
            database.abortNextWrite = false;
            aborted = true;
            break;
          }
          if (operation.type === 'put') staged.set(operation.key, operation.value);
          else staged.delete(operation.key);
        }
        if (commitGate) {
          commitGate.enter();
          await commitGate.released;
        }
        if (aborted) transaction.onabort?.();
        else {
          if (mode === 'readwrite') database.state = staged;
          transaction.oncomplete?.();
        }
      } finally {
        releaseTurn();
      }
    });
    return transaction;
  }
}

class ProbeIndexedDB {
  constructor() {
    this.database = new MemoryDatabase();
    this.created = false;
    this.openCount = 0;
    this.deleteCount = 0;
    this.persistedProbe = null;
    this.database.objectStoreNames = {
      contains: () => this.created,
    };
    this.database.createObjectStore = () => { this.created = true; };
    this.database.close = () => {
      if (!this.persistedProbe && this.database.state.has('probe')) {
        this.persistedProbe = clone(this.database.state.get('probe'));
      }
    };
  }

  open() {
    const request = {
      result: this.database,
      onupgradeneeded: null,
      onsuccess: null,
      onerror: null,
      onblocked: null,
    };
    queueMicrotask(() => {
      this.openCount += 1;
      if (!this.created) request.onupgradeneeded?.();
      request.onsuccess?.();
    });
    return request;
  }

  deleteDatabase() {
    const request = { onsuccess: null, onerror: null, onblocked: null };
    queueMicrotask(() => {
      this.deleteCount += 1;
      this.database.state.clear();
      request.onsuccess?.();
    });
    return request;
  }
}

class MemoryLockManager {
  constructor() {
    this.held = new Set();
  }

  request(name, options, callback) {
    return new Promise((resolve, reject) => {
      queueMicrotask(async () => {
        if (options?.mode !== 'exclusive' || options?.ifAvailable !== true) {
          reject(new Error('invalid_lock_request'));
          return;
        }
        if (this.held.has(name)) {
          try { resolve(await callback(null)); } catch (error) { reject(error); }
          return;
        }
        this.held.add(name);
        try {
          resolve(await callback({ name, mode: 'exclusive' }));
        } catch (error) {
          reject(error);
        } finally {
          this.held.delete(name);
        }
      });
    });
  }
}

function fakeDocument() {
  const elements = new Map();
  function makeElement() {
    const listeners = new Map();
    return {
      children: [],
      className: '',
      disabled: false,
      hidden: true,
      textContent: '',
      type: '',
      value: '',
      addEventListener(name, listener) { listeners.set(name, listener); },
      activate(name = 'click', event = {}) {
        return listeners.get(name)?.({ preventDefault() {}, target: this, ...event });
      },
      append(...children) { this.children.push(...children); },
      replaceChildren(...children) { this.children = children; },
    };
  }
  return {
    body: { dataset: {} },
    createElement() { return makeElement(); },
    getElementById(id) {
      if (!elements.has(id)) elements.set(id, makeElement());
      return elements.get(id);
    },
  };
}

function pairing() {
  return {
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    trusted: true,
  };
}

function pending(overrides = {}) {
  return {
    request_key: `remote_web_${'B'.repeat(32)}`,
    url: 'https://public.example/article',
    job_id: null,
    item_id: null,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    state: 'pending_delivery',
    ...overrides,
  };
}

test('browser WebCrypto exactly matches the checked-in Node and Swift fixture', async () => {
  const browser = await import('../web/protocol.mjs');
  const subtle = webcrypto.subtle;
  const transcript = browser.handshakeTranscript(fixture.fields);
  assert.equal(browser.base64url(transcript), fixture.transcript);
  assert.equal(browser.base64url(await browser.bootstrapProof(
    fixture.seeds.bootstrap_secret,
    transcript,
    fixture.fields.phone_identity_public_key,
    subtle,
  )), fixture.bootstrap_proof);

  assert.equal(await browser.verifyTranscript(
    browser.decodeBase64url(fixture.fields.mac_identity_public_key, 32),
    transcript,
    browser.decodeBase64url(fixture.mac_signature, 64),
    false,
    subtle,
  ), true);
  const phoneIdentity = await subtle.importKey(
    'pkcs8',
    privatePkcs8(fixture.seeds.phone_identity, '70'),
    'Ed25519',
    false,
    ['sign'],
  );
  assert.equal(browser.base64url(await browser.signTranscript(
    phoneIdentity, transcript, subtle,
  )), fixture.phone_signature);

  const phoneEphemeral = await subtle.importKey(
    'pkcs8',
    privatePkcs8(fixture.seeds.phone_ephemeral, '6e'),
    'X25519',
    false,
    ['deriveBits'],
  );
  const keyBytes = await browser.deriveDirectionalKeyBytes(
    phoneEphemeral,
    browser.decodeBase64url(fixture.fields.mac_ephemeral_public_key, 32),
    transcript,
    fixture.fields,
    browser.decodeBase64url(fixture.bootstrap_proof, 32),
    subtle,
  );
  assert.equal(browser.base64url(keyBytes.phoneToMac), fixture.phone_to_mac_key);
  assert.equal(browser.base64url(keyBytes.macToPhone), fixture.mac_to_phone_key);

  const keys = await browser.importDirectionalKeys(keyBytes, subtle);
  const envelope = await browser.encryptEnvelope({
    key: keys.phoneToMac,
    sessionId: fixture.fields.session_id,
    sender: 'iphone',
    counter: 0n,
    plaintext: JSON.stringify(fixture.rpc),
    subtle,
  });
  assert.deepEqual(envelope, fixture.envelope);
  const decryptKey = await subtle.importKey(
    'raw', keyBytes.phoneToMac, 'AES-GCM', false, ['decrypt'],
  );
  assert.deepEqual(JSON.parse(new TextDecoder().decode(await browser.decryptEnvelope({
    key: decryptKey,
    envelope: fixture.envelope,
    sessionId: fixture.fields.session_id,
    sender: 'iphone',
    expectedCounter: 0n,
    subtle,
  }))), fixture.rpc);
  await assert.rejects(() => browser.decryptEnvelope({
    key: decryptKey,
    envelope: fixture.envelope,
    sessionId: fixture.fields.session_id,
    sender: 'iphone',
    expectedCounter: 1n,
    subtle,
  }), /invalid_envelope/);
});

test('in-page pairing code pins the stable web route and relay without becoming a URL', async () => {
  const browser = await import('../web/protocol.mjs');
  const root = Buffer.from(fixture.seeds.admission_root, 'base64url');
  const now = Date.now();
  const webClient = 'https://web.example.test/app';
  const relay = 'wss://relay.example.test/relay';
  const transportConfig = { webClientUrl: webClient, relayUrl: relay };
  const state = {
    session_id: fixture.fields.session_id,
    generation: fixture.fields.pairing_generation,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    bootstrap_secret: fixture.seeds.bootstrap_secret,
    pairing_expires_at_ms: now + 60_000,
  };
  assert.equal(diagnosticWebClientUrl('wss://review.trycloudflare.com/relay').toString(), 'https://review.trycloudflare.com/');
  assert.equal(parseWebClientUrl(webClient).toString(), webClient);
  const code = browserPairingCode(state, relay, root, webClient);
  assert.match(code, /^MEMEX PAIR 1:[A-Za-z0-9_-]+$/);
  assert.equal(code.includes('://'), false);
  assert.equal(code.includes(webClient), false);
  const scanned = browser.parsePairingCode(code, webClient, transportConfig, now);
  assert.equal(scanned.web_client_url, webClient);
  assert.equal(scanned.relay, relay);
  assert.equal(scanned.bootstrap_secret, state.bootstrap_secret);
  assert.equal(scanned.iphone_admission_capability, fixture.iphone_admission_capability);
  assert.throws(() => browser.parsePairingCode(
    browserPairingCode({ ...state, pairing_expires_at_ms: now + (30 * 60_000) + 30_001 },
      relay, root, webClient),
    webClient, transportConfig, now,
  ), /invalid_pairing/);
  assert.throws(() => browser.parsePairingCode(code, webClient, transportConfig, now + 90_001),
    /invalid_pairing/);

  assert.equal(browser.relaySocketUrl(scanned), `wss://relay.example.test/relay/${state.session_id}`);
  assert.deepEqual(browser.browserSubprotocols(scanned), [
    'memex.remote.v1', 'memex.role.iphone', 'memex.client.browser',
    `memex.cap.${fixture.iphone_admission_capability}`,
  ]);
  assert.equal(browser.relaySocketUrl(scanned).includes(scanned.iphone_admission_capability), false);
  assert.equal(browser.relaySocketUrl(scanned).includes(scanned.bootstrap_secret), false);
  assert.equal(
    browser.parsePairingCode(code, `${webClient}/settings`, transportConfig, now).session_id,
    state.session_id,
  );

  const nativeCode = nativePairingCode(
    state, relay, root,
  );
  assert.equal(nativeCode.startsWith('MEMEX1:'), true);
  const nativePayload = JSON.parse(Buffer.from(nativeCode.slice('MEMEX1:'.length), 'base64url'));
  assert.equal(
    nativePayload.iphone_admission_capability,
    fixture.native_iphone_admission_capability,
  );
  assert.notEqual(
    nativePayload.iphone_admission_capability,
    scanned.iphone_admission_capability,
  );

  for (const invalid of [
    'https://web.example.test/app?relay=bad',
    'https://user@web.example.test/app',
    'https://web.example.test/app#bad',
    'https://web.example.test/app/',
    'http://web.example.test/app',
  ]) assert.throws(() => parseWebClientUrl(invalid), /invalid_web_client_url/);
});

test('fragment navigation fails before browser lease, crypto probe, or storage', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const calls = [];
  const app = new BrowserApp({
    fragment: `#MEMEX1:${'A'.repeat(64)}`,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: fakeDocument(),
    acquireLease: async () => { calls.push('lease'); },
    capabilityProbe: async () => { calls.push('probe'); },
    databaseOpen: async () => { calls.push('database'); },
    webClientUrl: 'https://review.example/app',
    relayUrl: 'wss://relay.review.example/relay',
  });

  await assert.rejects(() => app.initialize(), /invalid_navigation/);
  assert.deepEqual(calls, []);
  assert.equal(app.fragment, `#MEMEX1:${'A'.repeat(64)}`);
});

test('authenticated unpaired app keeps scanned bootstrap memory-only until exact trust commits', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const root = Buffer.from(fixture.seeds.admission_root, 'base64url');
  const now = Date.now();
  const webClient = 'https://web.example.test/app';
  const relay = 'wss://relay.example.test/relay';
  const code = browserPairingCode({
    session_id: fixture.fields.session_id,
    generation: fixture.fields.pairing_generation,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    bootstrap_secret: fixture.seeds.bootstrap_secret,
    pairing_expires_at_ms: now + 60_000,
  }, relay, root, webClient);
  const database = new MemoryDatabase();
  const documentValue = fakeDocument();
  const scans = [];
  const socket = { addEventListener() {} };
  const app = new BrowserApp({
    fragment: '',
    hadQuery: false,
    pageHref: `${webClient}/settings`,
    documentValue,
    acquireLease: async () => ({ release: async () => {} }),
    capabilityProbe: async () => {},
    databaseOpen: async () => database,
    scanPairing: async (options) => { scans.push(options); return code; },
    channelOpen: async ({ pairing, persistTrust }) => {
      await persistTrust({
        ...pairing,
        bootstrap_secret: null,
        expires_at_ms: 0,
        trusted: true,
      });
      return { socket, async request() {} };
    },
    webClientUrl: webClient,
    relayUrl: relay,
  });

  await app.initialize();
  assert.equal(documentValue.getElementById('unpaired').hidden, false);
  assert.equal(database.state.has('pairing'), false);
  await app.beginPairingScan();
  clearTimeout(app.connectTimer);
  assert.equal(scans.length, 1);
  assert.equal(scans[0].signal.aborted, false);
  assert.equal(documentValue.getElementById('pairing-scanner').hidden, true);
  assert.equal(documentValue.getElementById('capture').hidden, true);
  assert.equal(database.state.has('pairing'), false);
  assert.equal(app.pairing.trusted, false);
  assert.equal(app.pairing.session_id, fixture.fields.session_id);
  assert.equal(app.pairing.bootstrap_secret, fixture.seeds.bootstrap_secret);
  assert.equal(app.pairing.web_client_url, webClient);
  assert.equal(app.pairing.relay, relay);

  await app.connect();
  const stored = database.state.get('pairing');
  assert.equal(stored.trusted, true);
  assert.equal(stored.bootstrap_secret, null);
  assert.equal(stored.expires_at_ms, 0);
  assert.equal(documentValue.getElementById('capture').hidden, false);
});

test('legacy untrusted bootstrap is deleted and never resumed from browser storage', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const webClient = 'https://web.example.test/app';
  const relay = 'wss://relay.example.test/relay';
  const database = new MemoryDatabase({ pairing: {
    v: 1,
    relay,
    session_id: fixture.fields.session_id,
    pairing_generation: fixture.fields.pairing_generation,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    bootstrap_secret: fixture.seeds.bootstrap_secret,
    iphone_admission_capability: fixture.iphone_admission_capability,
    expires_at_ms: Date.now() - 1,
    trusted: false,
    web_client_url: webClient,
  } });
  const documentValue = fakeDocument();
  const app = new BrowserApp({
    fragment: '',
    hadQuery: false,
    pageHref: `${webClient}/settings`,
    documentValue,
    acquireLease: async () => ({ release: async () => {} }),
    capabilityProbe: async () => {},
    databaseOpen: async () => database,
    webClientUrl: webClient,
    relayUrl: relay,
  });

  await app.initialize();
  assert.equal(database.state.has('pairing'), false);
  assert.equal(app.pairing, null);
  assert.equal(documentValue.getElementById('unpaired').hidden, false);
  assert.equal(documentValue.getElementById('capture').hidden, true);
});

test('an untrusted in-memory bootstrap returns to scanning at expiry without persistence', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const root = Buffer.from(fixture.seeds.admission_root, 'base64url');
  const webClient = 'https://web.example.test/app';
  const relay = 'wss://relay.example.test/relay';
  const code = browserPairingCode({
    session_id: fixture.fields.session_id,
    generation: fixture.fields.pairing_generation,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    bootstrap_secret: fixture.seeds.bootstrap_secret,
    pairing_expires_at_ms: Date.now() - 1,
  }, relay, root, webClient);
  const database = new MemoryDatabase();
  const documentValue = fakeDocument();
  const app = new BrowserApp({
    fragment: '',
    hadQuery: false,
    pageHref: `${webClient}/settings`,
    documentValue,
    acquireLease: async () => ({ release: async () => {} }),
    capabilityProbe: async () => {},
    databaseOpen: async () => database,
    scanPairing: async () => code,
    channelOpen: async () => { throw new Error('mac_offline'); },
    webClientUrl: webClient,
    relayUrl: relay,
  });

  await app.initialize();
  await app.beginPairingScan();
  clearTimeout(app.connectTimer);
  await app.connect();
  assert.equal(app.pairing, null);
  assert.equal(database.state.has('pairing'), false);
  assert.equal(documentValue.getElementById('unpaired').hidden, false);
  assert.equal(documentValue.getElementById('capture').hidden, true);
});

test('browser status, one-outbox, URL, and local-stop semantics stay explicit and bounded', async () => {
  const {
    BrowserApp, canResumeOutbox, normalizeStoredOutbox, statusText, validateCaptureUrl,
    validateOutbox,
  } = await import('../web/app.mjs');
  assert.match(statusText('needs_user'), /需要在 Mac 上批准/);
  assert.match(statusText('pending_delivery'), /尚未受理/);
  assert.match(statusText('succeeded'), /存储成功/);
  for (const state of [
    'pending_delivery', 'queued', 'leased', 'artifact_ready', 'retry_wait', 'needs_user',
    'confirmation_required', 'stopping', 'stop_error', 'succeeded', 'failed', 'cancelled',
    'unknown',
  ]) assert.match(statusText(state), /[\u3400-\u9fff]/, state);

  const errorDocument = fakeDocument();
  const errorApp = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: errorDocument,
  });
  for (const code of [
    'invalid_navigation', 'invalid_pairing', 'scanner_unavailable', 'camera_unavailable',
    'scan_timeout', 'invalid_url', 'pending_exists', 'mac_offline',
    'daemon_unavailable', 'request_identity_conflict', 'remote_resource_not_found',
    'result_too_large', 'library_scan_limit_exceeded', 'another_tab_active',
    'outbox_conflict', 'storage_unavailable',
    'unsupported_secure_context', 'unsupported_webcrypto', 'unsupported_web_locks',
    'unsupported_ed25519', 'unsupported_x25519', 'unsupported_hkdf_aes_gcm',
    'unsupported_indexeddb_write', 'unsupported_indexeddb_reopen',
    'unsupported_indexeddb_cryptokey', 'unsupported_browser', 'unknown_private_error',
  ]) {
    errorApp.setError(new Error(code));
    assert.match(errorDocument.getElementById('error-message').textContent, /[\u3400-\u9fff]/, code);
  }
  assert.equal(validateCaptureUrl('https://public.example/article'), 'https://public.example/article');
  assert.throws(() => validateCaptureUrl('http://public.example/article'), /invalid_url/);
  assert.throws(() => validateCaptureUrl('https://user:pass@public.example/'), /invalid_url/);
  assert.throws(() => validateCaptureUrl('https://public.example/#private'), /invalid_url/);
  assert.deepEqual(validateOutbox({
    request_key: `remote_web_${'A'.repeat(32)}`,
    url: 'https://public.example/article',
    job_id: null,
    item_id: null,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    state: 'pending_delivery',
  }).job_id, null);
  assert.throws(() => validateOutbox({
    request_key: `remote_web_${'A'.repeat(32)}`,
    url: 'https://public.example/article',
    job_id: null,
    item_id: null,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    state: 'pending_delivery',
    extra: true,
  }), /storage_unavailable/);

  const bound = {
    request_key: `remote_web_${'B'.repeat(32)}`,
    url: 'https://public.example/article',
    job_id: 'job_00000000000000000000000000000000',
    item_id: 'item_bound',
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    state: 'queued',
  };
  const sameMacNewSession = {
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
  };
  assert.equal(normalizeStoredOutbox(bound, sameMacNewSession).state, 'queued');
  const differentMac = {
    mac_device_id: '99999999-9999-4999-8999-999999999999',
    mac_identity_public_key: 'C'.repeat(43),
  };
  const blocked = normalizeStoredOutbox(bound, differentMac);
  assert.equal(blocked.state, 'confirmation_required');
  assert.equal(blocked.url, bound.url);
  assert.equal(blocked.job_id, bound.job_id);
  const transmitted = [];
  if (canResumeOutbox(blocked, differentMac)) {
    transmitted.push(blocked.url, blocked.job_id, blocked.item_id);
  }
  assert.deepEqual(transmitted, []);
  assert.equal(canResumeOutbox(bound, sameMacNewSession), true);
  const legacy = { ...bound };
  delete legacy.mac_device_id;
  delete legacy.mac_identity_public_key;
  const migrated = normalizeStoredOutbox(legacy, sameMacNewSession);
  assert.equal(migrated.state, 'confirmation_required');
  assert.equal(migrated.mac_device_id, null);
  assert.equal(migrated.mac_identity_public_key, null);
});

test('live library navigation and result schemas are exact and bounded', async () => {
  const {
    validateLibraryItemResult, validateLibraryListResult, validateLibraryPage,
    validateSettingsPage,
  } = await import('../web/app.mjs');

  const home = 'https://web.example.test/app';
  assert.doesNotThrow(() => validateLibraryPage(home, home));
  assert.doesNotThrow(() => validateSettingsPage(`${home}/settings`, home));
  for (const invalid of [
    `${home}/settings`, `${home}/account`, `${home}?limit=50`, `${home}#pair`,
    'https://other.example/app',
  ]) assert.throws(() => validateLibraryPage(invalid, home), /invalid_navigation/);
  for (const invalid of [home, `${home}/settings?pair=1`, `${home}/settings#pair`]) {
    assert.throws(() => validateSettingsPage(invalid, home), /invalid_navigation/);
  }

  const page = {
    items: [{
      item_id: 'item_fixture', revision_id: 'revision_fixture', title: '合成资料',
      item_type: 'webpage', captured_at: '2026-01-01T00:00:00Z', site_name: '合成来源',
    }],
    next_cursor: 'item_fixture',
  };
  assert.equal(validateLibraryListResult(page), page);
  assert.throws(() => validateLibraryListResult({
    ...page, items: [{ ...page.items[0], source_url: 'https://private.invalid/' }],
  }), /invalid_response/);
  assert.throws(() => validateLibraryListResult({
    items: Array.from({ length: 51 }, () => page.items[0]), next_cursor: null,
  }), /invalid_response/);
  assert.throws(() => validateLibraryListResult({
    items: [{ ...page.items[0], title: 'x'.repeat(4 * 1024 + 1) }], next_cursor: null,
  }), /invalid_response/);
  assert.throws(() => validateLibraryListResult({
    items: [{ ...page.items[0], captured_at: '2026-01-01' }], next_cursor: null,
  }), /invalid_response/);
  assert.deepEqual(validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'succeeded', summary: '经过校验的摘要',
    translation_state: 'succeeded', title: '合成资料', markdown: '# 安全文本',
  }, 'item_fixture').summary, '经过校验的摘要');
  assert.throws(() => validateLibraryItemResult({
    item_id: 'item_other', revision_id: 'revision_fixture',
    summary_state: 'not_requested', summary: null,
    translation_state: 'succeeded', title: '合成资料', markdown: '# 安全文本',
  }, 'item_fixture'), /invalid_response/);
  for (const translation_state of [
    'not_requested', 'queued', 'running', 'failed', 'unavailable',
  ]) assert.equal(validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'not_requested', summary: null,
    translation_state, title: null, markdown: null,
  }, 'item_fixture').translation_state, translation_state);
  for (const summary_state of [
    'not_requested', 'queued', 'running', 'failed', 'unavailable',
  ]) assert.equal(validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state, summary: null,
    translation_state: 'not_requested', title: null, markdown: null,
  }, 'item_fixture').summary_state, summary_state);
  assert.throws(() => validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'not_requested', summary: null,
    translation_state: 'queued', title: 'source leak', markdown: null,
  }, 'item_fixture'), /invalid_response/);
  assert.throws(() => validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'queued', summary: 'source leak',
    translation_state: 'not_requested', title: null, markdown: null,
  }, 'item_fixture'), /invalid_response/);
  assert.throws(() => validateLibraryItemResult({
    item_id: 'item_fixture', revision_id: 'revision_fixture',
    summary_state: 'succeeded', summary: '摘'.repeat(8 * 1024 + 1),
    translation_state: 'succeeded', title: '合成标题'.repeat(1024),
    markdown: '译'.repeat(78 * 1024),
  }, 'item_fixture'), /invalid_response/);
});

test('paired live library reuses durable identity, lists, and reads safe text over one channel', async () => {
  const { LibraryApp } = await import('../web/app.mjs');
  const protocol = await import('../web/protocol.mjs');
  const identity = await protocol.generateIdentity(webcrypto.subtle);
  const storedPairing = {
    v: 1,
    web_client_url: 'https://web.example.test/app',
    relay: 'wss://relay.example.test/relay',
    session_id: fixture.fields.session_id,
    pairing_generation: fixture.fields.pairing_generation,
    mac_device_id: fixture.fields.mac_device_id,
    mac_identity_public_key: fixture.fields.mac_identity_public_key,
    bootstrap_secret: null,
    iphone_admission_capability: fixture.iphone_admission_capability,
    expires_at_ms: fixture.fields.expires_at_ms,
    trusted: true,
  };
  const database = new MemoryDatabase({
    pairing: storedPairing,
    identity,
    phone_device_id: 'web_AAAAAAAAAAAAAAAAAAAAAA',
  });
  const requests = [];
  const channelArguments = [];
  const closeListeners = [];
  const deferredReads = new Map();
  let responseMode = 'items';
  const channel = {
    socket: { addEventListener(name, listener) { if (name === 'close') closeListeners.push(listener); } },
    request: async (_id, method, params) => {
      requests.push({ method, params });
      if (responseMode === 'error') return { error: { code: 'daemon_unavailable' } };
      if (responseMode === 'deferred_read' && method === 'memex.library.item.read') {
        return new Promise((resolve) => deferredReads.set(params.item_id, resolve));
      }
      if (method === 'memex.library.list') return { result: {
        items: responseMode === 'empty' ? [] : [{
          item_id: 'item_fixture', revision_id: 'revision_fixture', title: '<b>合成资料</b>',
          item_type: 'webpage', captured_at: '2026-01-01T00:00:00Z', site_name: '合成来源',
        }],
        next_cursor: responseMode === 'first_page' ? 'item_fixture' : null,
      } };
      return { result: {
        item_id: 'item_fixture', revision_id: 'revision_fixture',
        summary_state: 'succeeded', summary: '<b>校验摘要</b>',
        translation_state: 'succeeded',
        title: '<b>校验翻译</b>', markdown: '<script>not executable translated text</script>',
      } };
    },
  };
  const documentValue = fakeDocument();
  documentValue.getElementById('library').hidden = false;
  const app = new LibraryApp({
    pageHref: 'https://web.example.test/app',
    documentValue,
    acquireLease: async () => ({ release: async () => {} }),
    capabilityProbe: async () => {},
    databaseOpen: async () => database,
    channelOpen: async (options) => { channelArguments.push(options); return channel; },
  });
  app.scheduleConnect = () => {};

  await app.initialize();
  assert.equal(app.identity.publicKey, identity.publicKey);
  assert.equal(app.phoneId, 'web_AAAAAAAAAAAAAAAAAAAAAA');
  assert.equal(documentValue.getElementById('library').hidden, false);
  assert.equal(documentValue.getElementById('connection-state').textContent, 'Mac 离线');
  responseMode = 'error';
  await app.connect();
  assert.equal(documentValue.getElementById('connection-state').textContent, 'Mac 在线');
  assert.match(documentValue.getElementById('library-state').textContent, /加载失败/);
  assert.equal(documentValue.getElementById('library-count').textContent, '不可用');
  assert.equal(app.channel, channel);
  responseMode = 'items';
  await app.loadPage(true);
  assert.equal(documentValue.getElementById('connection-state').textContent, 'Mac 在线');
  assert.equal(documentValue.getElementById('library-list').children.length, 1);
  assert.equal(documentValue.getElementById('library-count').textContent, '1');
  assert.equal(documentValue.getElementById('library-list').children[0].children[0].textContent, '<b>合成资料</b>');
  documentValue.getElementById('library-search').value = '无匹配';
  documentValue.getElementById('library-search').activate('input');
  assert.equal(documentValue.getElementById('library-list').children.length, 0);
  assert.equal(documentValue.getElementById('library-count').textContent, '0');
  documentValue.getElementById('library-search').value = '合成来源';
  documentValue.getElementById('library-search').activate('input');
  assert.equal(documentValue.getElementById('library-list').children.length, 1);
  documentValue.getElementById('theme-button').activate();
  assert.equal(documentValue.body.dataset.theme, 'dark');
  const summary = app.items[0];
  await app.readItem(summary);
  assert.equal(documentValue.getElementById('library-item-title').textContent, '<b>校验翻译</b>');
  assert.equal(documentValue.getElementById('library-item-summary').textContent, '<b>校验摘要</b>');
  assert.equal(documentValue.getElementById('library-item-body').textContent, '<script>not executable translated text</script>');
  assert.equal(documentValue.getElementById('library-view').hidden, true);
  assert.match(documentValue.getElementById('library-state').textContent, /当前版本/);
  documentValue.getElementById('reader-back').activate();
  assert.equal(documentValue.getElementById('library-view').hidden, false);
  assert.equal(documentValue.getElementById('library-item').hidden, true);
  assert.deepEqual(requests, [
    { method: 'memex.library.list', params: { cursor: null } },
    { method: 'memex.library.list', params: { cursor: null } },
    { method: 'memex.library.item.read', params: { item_id: 'item_fixture' } },
  ]);
  assert.equal(channelArguments.length, 1);
  assert.deepEqual(Object.keys(channelArguments[0]).sort(), [
    'allowLocal', 'identity', 'pairing', 'persistTrust', 'phoneId',
  ]);
  assert.equal(JSON.stringify(channelArguments[0]).includes('cookie'), false);
  assert.equal(JSON.stringify(channelArguments[0]).includes('jwt'), false);
  assert.equal(closeListeners.length, 1);

  const firstSummary = {
    item_id: 'item_first', revision_id: 'revision_first', title: 'First',
    item_type: 'webpage', captured_at: null, site_name: 'Fixture',
  };
  const secondSummary = {
    item_id: 'item_second', revision_id: 'revision_second', title: 'Second',
    item_type: 'webpage', captured_at: null, site_name: 'Fixture',
  };
  responseMode = 'deferred_read';
  const firstRead = app.readItem(firstSummary);
  const secondRead = app.readItem(secondSummary);
  deferredReads.get('item_second')({ result: {
    item_id: 'item_second', revision_id: 'revision_second',
    summary_state: 'succeeded', summary: 'Second summary',
    translation_state: 'succeeded', title: 'Second translated', markdown: 'Second body',
  } });
  await secondRead;
  deferredReads.get('item_first')({ result: {
    item_id: 'item_first', revision_id: 'revision_first',
    summary_state: 'succeeded', summary: 'Stale first summary',
    translation_state: 'succeeded', title: 'Stale first translated', markdown: 'Stale first body',
  } });
  await firstRead;
  assert.equal(documentValue.getElementById('library-item-title').textContent, 'Second translated');
  assert.equal(documentValue.getElementById('library-item-summary').textContent, 'Second summary');
  assert.equal(documentValue.getElementById('library-item-body').textContent, 'Second body');

  responseMode = 'first_page';
  await app.loadPage(true);
  assert.equal(documentValue.getElementById('library-count').textContent, '1+');
  responseMode = 'error';
  await app.loadPage(false);
  assert.equal(documentValue.getElementById('library-count').textContent, '可能过时');
  assert.match(documentValue.getElementById('library-state').textContent, /可能不完整或过时/);

  responseMode = 'empty';
  await app.loadPage(true);
  assert.match(documentValue.getElementById('library-state').textContent, /没有可显示的资料/);
  responseMode = 'error';
  await app.loadPage(true);
  assert.match(documentValue.getElementById('library-state').textContent, /加载失败/);
  assert.equal(documentValue.getElementById('library-count').textContent, '不可用');
  assert.equal(documentValue.getElementById('library-list').children.length, 0);
  assert.match(documentValue.getElementById('error-message').textContent, /Mac 采集服务/);
  await assert.rejects(app.readItem(summary), /daemon_unavailable/);
  assert.equal(documentValue.getElementById('library-item').hidden, true);
  assert.equal(documentValue.getElementById('library-item-body').textContent, '');
  closeListeners[0]();
  assert.equal(documentValue.getElementById('connection-state').textContent, 'Mac 离线');
  assert.match(documentValue.getElementById('library-state').textContent, /Mac 离线/);

  let resolveOldList;
  const oldChannel = {
    request: async () => new Promise((resolve) => { resolveOldList = resolve; }),
  };
  const newChannel = {
    request: async () => ({ result: {
      items: [{
        item_id: 'item_new_channel', revision_id: 'revision_new_channel', title: 'New channel',
        item_type: 'webpage', captured_at: null, site_name: 'Fixture',
      }],
      next_cursor: null,
    } }),
  };
  app.channel = oldChannel;
  const oldLoad = app.loadPage(true);
  app.channel = newChannel;
  await app.loadPage(true);
  resolveOldList({ result: {
    items: [{
      item_id: 'item_stale_channel', revision_id: 'revision_stale_channel', title: 'Stale channel',
      item_type: 'webpage', captured_at: null, site_name: 'Fixture',
    }],
    next_cursor: null,
  } });
  await oldLoad;
  assert.deepEqual(app.items.map((item) => item.item_id), ['item_new_channel']);
});

test('live library reports another-tab contention without probing, storage, or a socket', async () => {
  const { LibraryApp } = await import('../web/app.mjs');
  const calls = [];
  const documentValue = fakeDocument();
  const app = new LibraryApp({
    pageHref: 'https://web.example.test/app',
    documentValue,
    acquireLease: async () => { throw new Error('another_tab_active'); },
    capabilityProbe: async () => { calls.push('probe'); },
    databaseOpen: async () => { calls.push('database'); },
    channelOpen: async () => { calls.push('channel'); },
  });
  let error;
  try { await app.initialize(); } catch (caught) { error = caught; }
  app.showUnsupported(error);
  assert.deepEqual(calls, []);
  assert.equal(documentValue.getElementById('connection-state').textContent, '另一标签页正在运行');
  assert.match(documentValue.getElementById('unsupported-message').textContent, /另一个标签页/);
});

test('aborted submit stays absent from memory and disk and cannot transmit after reconnect', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const database = new MemoryDatabase();
  database.abortNextWrite = true;
  const transmitted = [];
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: fakeDocument(),
  });
  app.database = database;
  app.pairing = pairing();
  app.channel = { request: async (...values) => { transmitted.push(values); } };
  const version = app.workVersion;

  await assert.rejects(
    () => app.submit('https://public.example/not-transmitted'),
    /storage_unavailable/,
  );
  assert.equal(app.outbox, null);
  assert.equal(app.workVersion, version);
  assert.equal(database.state.get('outbox'), undefined);
  assert.deepEqual(transmitted, []);

  app.channel = { request: async (...values) => { transmitted.push(values); } };
  await app.processOutbox();
  assert.deepEqual(transmitted, []);
});

test('aborted reconfirm keeps prior confirmation state and cannot transmit after reconnect', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const prior = pending({
    job_id: 'job_00000000000000000000000000000000',
    item_id: 'item_prior',
    state: 'confirmation_required',
  });
  const database = new MemoryDatabase({ outbox: prior });
  database.abortNextWrite = true;
  const transmitted = [];
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: fakeDocument(),
  });
  app.database = database;
  app.pairing = pairing();
  app.outbox = clone(prior);
  app.workVersion = 9;
  app.channel = { request: async (...values) => { transmitted.push(values); } };

  await assert.rejects(() => app.reconfirmForCurrentMac(), /storage_unavailable/);
  assert.deepEqual(app.outbox, prior);
  assert.equal(app.workVersion, 9);
  assert.deepEqual(database.state.get('outbox'), prior);
  assert.deepEqual(transmitted, []);

  app.channel = { request: async (...values) => { transmitted.push(values); } };
  await app.processOutbox();
  assert.deepEqual(transmitted, []);
});

test('gated reconfirm followed by Stop cannot resurrect or transmit online or after reconnect', async (t) => {
  const { BrowserApp } = await import('../web/app.mjs');
  for (const online of [true, false]) {
    await t.test(online ? 'online' : 'offline then reconnect', async () => {
      const prior = pending({
        job_id: 'job_00000000000000000000000000000000',
        item_id: 'item_prior',
        state: 'confirmation_required',
      });
      const database = new MemoryDatabase({ outbox: prior });
      const gate = database.gateNextCommit();
      const documentValue = fakeDocument();
      const transmitted = [];
      const app = new BrowserApp({
        fragment: null,
        hadQuery: false,
        pageHref: 'https://review.example/',
        documentValue,
      });
      app.database = database;
      app.pairing = pairing();
      app.outbox = clone(prior);
      app.workVersion = 12;
      if (online) {
        app.channel = { request: async (...values) => { transmitted.push(values); } };
      }

      const reconfirm = app.reconfirmForCurrentMac();
      assert.equal(documentValue.getElementById('submit-button').disabled, true);
      assert.equal(documentValue.getElementById('reconfirm-button').disabled, true);
      assert.equal(documentValue.getElementById('cancel-button').disabled, true);
      await gate.entered;
      const stop = app.stopLocally();
      assert.equal(app.outbox.state, 'stopping');
      assert.equal(app.workVersion, 13);
      gate.release();
      await Promise.all([reconfirm, stop]);

      assert.equal(app.outbox, null);
      assert.equal(app.outboxTransition, null);
      assert.equal(database.state.get('outbox'), undefined);
      assert.equal(documentValue.getElementById('submit-button').disabled, false);
      assert.deepEqual(transmitted, []);

      app.channel = { request: async (...values) => { transmitted.push(values); } };
      await app.processOutbox();
      assert.deepEqual(transmitted, []);
    });
  }
});

test('gated submit followed by Stop cannot publish, persist, or transmit', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const database = new MemoryDatabase();
  const gate = database.gateNextCommit();
  const documentValue = fakeDocument();
  const transmitted = [];
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue,
  });
  app.database = database;
  app.pairing = pairing();
  app.channel = { request: async (...values) => { transmitted.push(values); } };

  const submit = app.submit('https://public.example/not-transmitted-after-stop');
  assert.equal(documentValue.getElementById('submit-button').disabled, true);
  assert.equal(documentValue.getElementById('cancel-button').disabled, true);
  await gate.entered;
  const stop = app.stopLocally();
  gate.release();
  await Promise.all([submit, stop]);

  assert.equal(app.outbox, null);
  assert.equal(app.outboxTransition, null);
  assert.equal(database.state.get('outbox'), undefined);
  assert.equal(documentValue.getElementById('submit-button').disabled, false);
  assert.deepEqual(transmitted, []);
  await app.processOutbox();
  assert.deepEqual(transmitted, []);
});

test('queued status save commits before Stop identity-delete and cannot resume', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const prior = pending();
  const queued = pending({
    job_id: 'job_00000000000000000000000000000000',
    state: 'queued',
  });
  const database = new MemoryDatabase({ outbox: prior });
  const gate = database.gateNextCommit();
  const transmitted = [];
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: fakeDocument(),
  });
  app.database = database;
  app.pairing = pairing();
  app.outbox = clone(prior);
  app.workVersion = 4;
  app.channel = { request: async (...values) => { transmitted.push(values); } };

  const save = app.saveOutbox(queued, prior.request_key, 4);
  await gate.entered;
  const stop = app.stopLocally();
  assert.equal(app.outbox.state, 'stopping');
  gate.release();
  assert.equal(await save, false);
  await stop;

  assert.equal(app.outbox, null);
  assert.equal(database.state.get('outbox'), undefined);
  app.channel = { request: async (...values) => { transmitted.push(values); } };
  await app.processOutbox();
  assert.deepEqual(transmitted, []);
});

test('failed Stop cleanup stays visibly fenced until explicit retry', async (t) => {
  const { BrowserApp, outboxOperationIdentity } = await import('../web/app.mjs');
  for (const mode of ['submit', 'reconfirm']) {
    await t.test(mode, async () => {
      const prior = mode === 'reconfirm' ? pending({ state: 'confirmation_required' }) : null;
      const database = new MemoryDatabase(prior ? { outbox: prior } : {});
      const gate = database.gateNextCommit();
      const transmitted = [];
      const app = new BrowserApp({
        fragment: null,
        hadQuery: false,
        pageHref: 'https://review.example/',
        documentValue: fakeDocument(),
      });
      app.database = database;
      app.pairing = pairing();
      app.outbox = prior ? clone(prior) : null;
      app.channel = { request: async (...values) => { transmitted.push(values); } };

      const transitionCall = mode === 'submit'
        ? app.submit('https://public.example/cleanup-abort')
        : app.reconfirmForCurrentMac();
      await gate.entered;
      const candidate = clone(app.outboxTransition.next);
      database.abortNextWrite = true;
      const stop = app.stopLocally();
      gate.release();
      await transitionCall;
      await assert.rejects(() => stop, /storage_unavailable/);

      assert.equal(app.outbox.state, 'stop_error');
      assert.deepEqual(
        outboxOperationIdentity(app.outbox), outboxOperationIdentity(candidate),
      );
      assert.deepEqual(
        outboxOperationIdentity(database.state.get('outbox')),
        outboxOperationIdentity(candidate),
      );
      assert.match(
        app.document.getElementById('job-status').textContent,
        /无法可靠停止/,
      );
      await app.processOutbox();
      assert.deepEqual(transmitted, []);

      await app.stopLocally();
      assert.equal(app.outbox, null);
      assert.equal(database.state.get('outbox'), undefined);
      assert.deepEqual(transmitted, []);
    });
  }
});

test('different operation identity survives stale Stop cleanup and remains fenced', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const { compareAndSetOutbox } = await import('../web/storage.mjs');
  const prior = pending({ state: 'confirmation_required' });
  const newer = pending({
    request_key: `remote_web_${'D'.repeat(32)}`,
    url: 'https://public.example/newer-operation',
  });
  const database = new MemoryDatabase({ outbox: prior });
  const gate = database.gateNextCommit();
  const transmitted = [];
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue: fakeDocument(),
  });
  app.database = database;
  app.pairing = pairing();
  app.outbox = clone(prior);
  app.channel = { request: async (...values) => { transmitted.push(values); } };

  const replace = compareAndSetOutbox(database, prior, newer);
  const stop = app.stopLocally();
  gate.release();
  await replace;
  await assert.rejects(() => stop, /outbox_conflict/);

  assert.equal(app.outbox.state, 'stop_error');
  assert.deepEqual(database.state.get('outbox'), newer);
  assert.deepEqual(transmitted, []);
  await app.processOutbox();
  assert.deepEqual(transmitted, []);
});

test('finish/delete and Stop serialize without resurrection or newer-operation deletion', async (t) => {
  const { BrowserApp } = await import('../web/app.mjs');
  const { compareAndSetOutbox } = await import('../web/storage.mjs');

  await t.test('finish delete then Stop leaves no durable operation', async () => {
    const prior = pending({ state: 'failed' });
    const database = new MemoryDatabase({ outbox: prior });
    const gate = database.gateNextCommit();
    const app = new BrowserApp({
      fragment: null,
      hadQuery: false,
      pageHref: 'https://review.example/',
      documentValue: fakeDocument(),
    });
    app.database = database;
    app.pairing = pairing();
    app.outbox = clone(prior);

    const finish = app.finishOutbox('Failed.');
    await gate.entered;
    const stop = app.stopLocally();
    assert.equal(app.outbox.state, 'stopping');
    gate.release();
    await Promise.all([finish, stop]);
    assert.equal(app.outbox, null);
    assert.equal(database.state.get('outbox'), undefined);
  });

  await t.test('newer identity ordered before finish and Stop survives both stale deletes', async () => {
    const prior = pending({ state: 'failed' });
    const newer = pending({
      request_key: `remote_web_${'E'.repeat(32)}`,
      url: 'https://public.example/newer-after-finish',
    });
    const database = new MemoryDatabase({ outbox: prior });
    const gate = database.gateNextCommit();
    const transmitted = [];
    const app = new BrowserApp({
      fragment: null,
      hadQuery: false,
      pageHref: 'https://review.example/',
      documentValue: fakeDocument(),
    });
    app.database = database;
    app.pairing = pairing();
    app.outbox = clone(prior);
    app.channel = { request: async (...values) => { transmitted.push(values); } };

    const replace = compareAndSetOutbox(database, prior, newer);
    await gate.entered;
    const finish = app.finishOutbox('Failed.');
    const stop = app.stopLocally();
    gate.release();
    await replace;
    await finish;
    await assert.rejects(() => stop, /outbox_conflict/);
    assert.deepEqual(database.state.get('outbox'), newer);
    assert.equal(app.outbox.state, 'stop_error');
    await app.processOutbox();
    assert.deepEqual(transmitted, []);
  });
});

test('exclusive browser lease rejects a second tab and releases after tab loss', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const { acquireBrowserLease } = await import('../web/storage.mjs');
  const locks = new MemoryLockManager();
  const first = await acquireBrowserLease(locks);
  let probed = false;
  let opened = false;
  let connected = false;
  const documentValue = fakeDocument();
  const second = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/settings',
    documentValue,
    acquireLease: () => acquireBrowserLease(locks),
    capabilityProbe: async () => { probed = true; },
    databaseOpen: async () => { opened = true; },
    webClientUrl: 'https://review.example/',
    relayUrl: 'wss://relay.review.example/relay',
  });
  second.scheduleConnect = () => { connected = true; };

  let contention;
  try {
    await second.initialize();
  } catch (error) {
    contention = error;
  }
  assert.equal(contention?.message, 'another_tab_active');
  second.showUnsupported(contention);
  assert.equal(documentValue.getElementById('connection-state').textContent, '另一标签页正在运行');
  assert.match(documentValue.getElementById('unsupported-message').textContent, /另一个标签页/);
  assert.equal(probed, false);
  assert.equal(opened, false);
  assert.equal(connected, false);
  assert.equal(second.outbox, null);

  await first.release();
  const recovered = await acquireBrowserLease(locks);
  assert.ok(recovered);
  await recovered.release();
});

test('outbox CAS rejects a stale tab after lease turnover', async () => {
  const { acquireBrowserLease, compareAndSetOutbox } = await import('../web/storage.mjs');
  const original = pending();
  const current = pending({ state: 'queued', job_id: 'job_00000000000000000000000000000000' });
  const stale = pending({
    request_key: `remote_web_${'C'.repeat(32)}`,
    url: 'https://public.example/stale',
  });
  const database = new MemoryDatabase({ outbox: original });
  const locks = new MemoryLockManager();

  const firstTab = await acquireBrowserLease(locks);
  await firstTab.release();
  const successorTab = await acquireBrowserLease(locks);
  await compareAndSetOutbox(database, original, current);
  await successorTab.release();
  const staleTab = await acquireBrowserLease(locks);
  await assert.rejects(
    () => compareAndSetOutbox(database, original, stale),
    /outbox_conflict/,
  );
  assert.deepEqual(database.state.get('outbox'), current);
  await staleTab.release();
});

test('capability probe reports bounded prerequisite and WebCrypto failure stages', async () => {
  const { runCapabilityProbe } = await import('../web/storage.mjs');
  await assert.rejects(() => runCapabilityProbe({
    crypto: webcrypto,
    indexedDB: null,
    locks: null,
    secureContext: true,
  }), /unsupported_web_locks/);
  await assert.rejects(() => runCapabilityProbe({
    crypto: webcrypto,
    indexedDB: null,
    locks: navigator.locks,
    secureContext: false,
  }), /unsupported_secure_context/);

  function cryptoFailing(algorithm) {
    return {
      getRandomValues: webcrypto.getRandomValues.bind(webcrypto),
      subtle: new Proxy(webcrypto.subtle, {
        get(target, property) {
          if (property === 'generateKey') {
            return async (value, ...rest) => {
              if (value?.name === algorithm) throw new Error('synthetic_failure');
              return target.generateKey(value, ...rest);
            };
          }
          const value = target[property];
          return typeof value === 'function' ? value.bind(target) : value;
        },
      }),
    };
  }

  await assert.rejects(() => runCapabilityProbe({
    crypto: cryptoFailing('Ed25519'),
    indexedDB: null,
    locks: navigator.locks,
    secureContext: true,
  }), /unsupported_ed25519/);
  await assert.rejects(() => runCapabilityProbe({
    crypto: cryptoFailing('X25519'),
    indexedDB: null,
    locks: navigator.locks,
    secureContext: true,
  }), /unsupported_x25519/);
});

test('capability probe persists only Ed25519 while exercising ephemeral X25519', async () => {
  const { runCapabilityProbe } = await import('../web/storage.mjs');
  const indexedDB = new ProbeIndexedDB();
  const calls = { xGenerate: 0, xDerive: 0 };
  const subtle = new Proxy(webcrypto.subtle, {
    get(target, property) {
      if (property === 'generateKey') {
        return async (algorithm, ...rest) => {
          if (algorithm?.name === 'X25519') calls.xGenerate += 1;
          return target.generateKey(algorithm, ...rest);
        };
      }
      if (property === 'deriveBits') {
        return async (algorithm, ...rest) => {
          if (algorithm?.name === 'X25519') calls.xDerive += 1;
          return target.deriveBits(algorithm, ...rest);
        };
      }
      const value = target[property];
      return typeof value === 'function' ? value.bind(target) : value;
    },
  });

  await runCapabilityProbe({
    crypto: {
      subtle,
      getRandomValues: webcrypto.getRandomValues.bind(webcrypto),
    },
    indexedDB,
    locks: { request() {} },
    secureContext: true,
  });

  assert.equal(indexedDB.openCount, 2);
  assert.equal(indexedDB.deleteCount, 2);
  assert.deepEqual(Object.keys(indexedDB.persistedProbe).sort(), ['edPrivate', 'edPublic']);
  assert.equal(indexedDB.persistedProbe.edPrivate.algorithm.name, 'Ed25519');
  assert.equal(indexedDB.persistedProbe.edPrivate.extractable, false);
  assert.equal(calls.xGenerate, 2);
  assert.equal(calls.xDerive, 2);
  assert.equal(indexedDB.database.state.size, 0);
});

test('browser shows only a bounded capability stage without exception details', async () => {
  const { BrowserApp } = await import('../web/app.mjs');
  const documentValue = fakeDocument();
  const app = new BrowserApp({
    fragment: null,
    hadQuery: false,
    pageHref: 'https://review.example/',
    documentValue,
  });
  app.showUnsupported(new Error('unsupported_indexeddb_cryptokey'));
  assert.equal(
    documentValue.getElementById('unsupported-message').textContent,
    '能力检查 S9 失败：无法再次使用已保存的不可提取密钥。',
  );
  app.showUnsupported(new Error('private_exception_detail'));
  assert.equal(
    documentValue.getElementById('unsupported-message').textContent,
    '无法完成安全请求。',
  );
});
