'use strict';

const assert = require('node:assert/strict');
const { randomBytes } = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const {
  KEY_EPOCH,
  PROTOCOL_VERSION,
  base64url,
  bootstrapProof,
  decryptEnvelope,
  deriveDirectionalKeys,
  encryptEnvelope,
  generateEphemeral,
  generateIdentity,
  handshakeTranscript,
  identityPrivateKey,
  signTranscript,
} = require('../protocol');
const { MacSecureSession } = require('../mac/secure-session');
const {
  MacKeychainStateStore, MemoryStateStore, beginFreshPairing,
} = require('../mac/state-store');

function clientHello(state, phoneIdentity, phoneEphemeral, phoneDeviceId, mode = 'qr_bootstrap') {
  return {
    kind: 'client_hello',
    protocol_version: PROTOCOL_VERSION,
    session_id: state.session_id,
    pairing_generation: state.generation,
    handshake_mode: mode,
    phone_device_id: phoneDeviceId,
    phone_identity_public_key: phoneIdentity.public_key,
    phone_ephemeral_public_key: base64url(phoneEphemeral.publicKey),
    client_nonce: base64url(randomBytes(32)),
  };
}

function fieldsFromServerHello(hello) {
  const { kind: _kind, mac_signature: _signature, ...fields } = hello;
  return fields;
}

function clientAuth(state, phoneIdentity, phoneDeviceId, fields, secret = null) {
  const transcript = handshakeTranscript(fields);
  const message = {
    kind: 'client_auth',
    session_id: state.session_id,
    phone_device_id: phoneDeviceId,
    key_epoch: KEY_EPOCH,
    phone_signature: signTranscript(
      identityPrivateKey(phoneIdentity.private_key_pkcs8), transcript, true,
    ),
  };
  if (secret) {
    message.bootstrap_proof = base64url(bootstrapProof(
      secret,
      transcript,
      fields.phone_identity_public_key,
    ));
  }
  return message;
}

async function establishedChannel(store, router, send) {
  const state = beginFreshPairing(store);
  const phoneIdentity = generateIdentity();
  const phoneEphemeral = generateEphemeral();
  const phoneDeviceId = '99999999-9999-4999-8999-999999999999';
  const session = new MacSecureSession({ stateStore: store, router, send });
  await session.receive(JSON.stringify(clientHello(
    state, phoneIdentity, phoneEphemeral, phoneDeviceId,
  )));
  const fields = fieldsFromServerHello(send.messages[0]);
  const transcript = handshakeTranscript(fields);
  const proof = bootstrapProof(
    state.bootstrap_secret, transcript, fields.phone_identity_public_key,
  );
  const keys = deriveDirectionalKeys(
    phoneEphemeral.privateKey,
    Buffer.from(fields.mac_ephemeral_public_key, 'base64url'),
    transcript,
    fields,
    proof,
  );
  await session.receive(JSON.stringify(clientAuth(
    state, phoneIdentity, phoneDeviceId, fields, state.bootstrap_secret,
  )));
  return { state, session, keys };
}

function collectingSend(callback) {
  const send = (message) => {
    callback?.(message, send.messages.length);
    send.messages.push(message);
  };
  send.messages = [];
  return send;
}

test('explicit thirty-minute bootstrap authenticates after five minutes, expires and is single-use', async () => {
  const { revokeTrust } = require('../mac/state-store');
  const { browserPairingCode } = require('../mac/bridge');
  const { parsePairingCode } = await import('../web/protocol.mjs');
  const store = new MemoryStateStore();
  const start = Date.now();
  const old = beginFreshPairing(store, start);
  assert.equal(old.pairing_expires_at_ms, start + 300000);
  revokeTrust(store);
  const state = beginFreshPairing(store, start, 1800000);
  const relay = 'wss://memex.example/relay';
  const webClientUrl = 'https://memex.example/mobile/index.html';
  const code = browserPairingCode(state, relay, base64url(randomBytes(32)), webClientUrl);
  assert.equal(parsePairingCode(code, webClientUrl, { relayUrl: relay, webClientUrl }, start).expires_at_ms, start + 1800000);
  const identity = generateIdentity(), ephemeral = generateEphemeral();
  const phone = '99999999-9999-4999-8999-999999999999';
  const send = collectingSend();
  const session = new MacSecureSession({stateStore:store, router:async()=>({}), send, now:()=>start + 1799999});
  await assert.rejects(session.receive(JSON.stringify(clientHello(old,identity,ephemeral,phone))));
  const expired = new MacSecureSession({stateStore:store, router:async()=>({}), send:()=>{}, now:()=>start + 1800000});
  await assert.rejects(expired.receive(JSON.stringify(clientHello(state,identity,ephemeral,phone))));
  const fresh = new MacSecureSession({stateStore:store, router:async()=>({}), send, now:()=>start + 1799999});
  await fresh.receive(JSON.stringify(clientHello(state,identity,ephemeral,phone)));
  const fields = fieldsFromServerHello(send.messages.at(-1));
  assert.equal(fields.expires_at_ms, state.pairing_expires_at_ms);
  await fresh.receive(JSON.stringify(clientAuth(state,identity,phone,fields,state.bootstrap_secret)));
  assert.equal(store.load().trusted_phone.phone_device_id, phone);
  assert.equal(store.load().pending_pairing, null);
  const reused = new MacSecureSession({stateStore:store, router:async()=>({}), send:()=>{}, now:()=>start + 1799999});
  await assert.rejects(reused.receive(JSON.stringify(clientHello(state,generateIdentity(),ephemeral,phone))));
  assert.throws(()=>beginFreshPairing(store,start,1800001),/invalid_pairing_duration/);
});

test('a bandwidth-delayed reply rechecks revocation before disclosure', async () => {
  const { SessionGrants } = require('../mac/library-router');
  const store = new MemoryStateStore();
  const send = collectingSend();
  const { session } = await establishedChannel(store, async () => ({}), send);
  const channel = session.channel;
  const grants = new SessionGrants();
  grants.set({ sessionId: channel.sessionId, generation: channel.generation,
    scopes: ['read'], maxSensitivity: 'public', expiresAt: Date.now() + 60000 });
  session.authorization = grants;
  const grant = grants.snapshot(channel);
  await session.reply(channel, grant, { result: 'x'.repeat(210000) });
  const sent = send.messages.length;
  const counter = channel.outboundCounter;
  const delayed = session.reply(channel, grant, { result: 'y'.repeat(210000) });
  await new Promise(r => setTimeout(r, 20));
  assert.equal(send.messages.length, sent, 'second large reply must wait for bandwidth');
  grants.revoke(channel.sessionId);
  await assert.rejects(delayed, /session_revoked/);
  assert.equal(send.messages.length, sent);
  assert.equal(channel.outboundCounter, counter);
});

function diskStateStore(root, timeoutMs = 3_000) {
  const statePath = path.join(root, 'synthetic-keychain.json');
  return new MacKeychainStateStore({
    lockPath: path.join(root, 'state.lock'),
    lockOptions: { timeoutMs },
    readState: () => {
      try { return JSON.parse(fs.readFileSync(statePath, 'utf8')); }
      catch (error) { if (error.code === 'ENOENT') return null; throw error; }
    },
    writeState: (state) => fs.writeFileSync(statePath, JSON.stringify(state), { mode: 0o600 }),
  });
}

function pairingChild(root) {
  const modulePath = path.join(__dirname, '../mac/state-store.js');
  return spawnSync(process.execPath, ['-e', `
    const fs = require('node:fs');
    const path = require('node:path');
    const { MacKeychainStateStore, beginFreshPairing } = require(${JSON.stringify(modulePath)});
    const root = ${JSON.stringify(root)};
    const statePath = path.join(root, 'synthetic-keychain.json');
    const store = new MacKeychainStateStore({
      lockPath: path.join(root, 'state.lock'), lockOptions: { timeoutMs: 0 },
      readState: () => JSON.parse(fs.readFileSync(statePath, 'utf8')),
      writeState: (state) => fs.writeFileSync(statePath, JSON.stringify(state), { mode: 0o600 }),
    });
    try { beginFreshPairing(store); process.stdout.write('rotated'); }
    catch (error) { process.stdout.write(error.message); }
  `], { encoding: 'utf8', timeout: 3_000 });
}

test('relay-observed handshake cannot bootstrap an attacker-selected phone without QR secret', async () => {
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const attackerIdentity = generateIdentity();
  const attackerEphemeral = generateEphemeral();
  const sent = [];
  const phoneDeviceId = '22222222-2222-4222-8222-222222222222';
  const session = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: (message) => sent.push(message),
  });
  await session.receive(JSON.stringify(clientHello(
    state, attackerIdentity, attackerEphemeral, phoneDeviceId,
  )));
  const fields = fieldsFromServerHello(sent[0]);
  const auth = clientAuth(state, attackerIdentity, phoneDeviceId, fields, randomBytes(32));
  await assert.rejects(session.receive(JSON.stringify(auth)), /invalid_handshake/);
  assert.equal(store.load().trusted_phone, null);
  assert.equal(store.load().pending_pairing.bootstrap_secret, state.bootstrap_secret);
});

test('fresh pairing invalidates an in-flight authenticated bootstrap before trust commit', async () => {
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const phoneIdentity = generateIdentity();
  const phoneEphemeral = generateEphemeral();
  const sent = [];
  const session = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: (message) => sent.push(message),
  });
  const phoneDeviceId = '33333333-3333-4333-8333-333333333333';
  await session.receive(JSON.stringify(clientHello(
    state, phoneIdentity, phoneEphemeral, phoneDeviceId,
  )));
  const fields = fieldsFromServerHello(sent[0]);
  beginFreshPairing(store);
  await assert.rejects(session.receive(JSON.stringify(clientAuth(
    state, phoneIdentity, phoneDeviceId, fields, state.bootstrap_secret,
  ))), /invalid_handshake/);
  assert.equal(store.load().trusted_phone, null);
});

test('fresh pairing preserves an established channel before its next RPC', async () => {
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const phoneIdentity = generateIdentity();
  const phoneEphemeral = generateEphemeral();
  const sent = [];
  let routed = false;
  let pairingConsumed = 0;
  const session = new MacSecureSession({
    stateStore: store,
    router: async () => { routed = true; return { id: 'rpc_revoked', result: {} }; },
    send: (message) => sent.push(message),
    onPairingConsumed: () => { pairingConsumed += 1; },
  });
  const phoneDeviceId = '44444444-4444-4444-8444-444444444444';
  await session.receive(JSON.stringify(clientHello(
    state, phoneIdentity, phoneEphemeral, phoneDeviceId,
  )));
  const fields = fieldsFromServerHello(sent[0]);
  const transcript = handshakeTranscript(fields);
  const proof = bootstrapProof(
    state.bootstrap_secret,
    transcript,
    fields.phone_identity_public_key,
  );
  const keys = deriveDirectionalKeys(
    phoneEphemeral.privateKey,
    Buffer.from(fields.mac_ephemeral_public_key, 'base64url'),
    transcript,
    fields,
    proof,
  );
  await session.receive(JSON.stringify(clientAuth(
    state, phoneIdentity, phoneDeviceId, fields, state.bootstrap_secret,
  )));
  assert.equal(store.load().pending_pairing, null);
  assert.equal(pairingConsumed, 1);
  beginFreshPairing(store);
  const envelope = encryptEnvelope({
    key: keys.phoneToMac,
    sessionId: state.session_id,
    sender: 'iphone',
    counter: 0n,
    plaintext: JSON.stringify({
      id: 'rpc_revoked',
      method: 'memex.capture.status',
      params: { job_id: 'job_00000000000000000000000000000000' },
    }),
  });
  await session.receive(JSON.stringify(envelope));
  assert.equal(routed, true);
  assert.notEqual(session.channel, null);
});

test('fresh pairing during a deferred owner RPC preserves both old-generation results and errors', async () => {
  for (const response of [
    { id: 'rpc_deferred', result: { markdown: 'SYNTHETIC_PRIVATE_RESULT_BODY' } },
    { id: 'rpc_deferred', error: { code: 'internal_error', message: 'internal_error' } },
  ]) {
    const store = new MemoryStateStore();
    let finishRouter;
    let routerStarted;
    const started = new Promise((resolve) => { routerStarted = resolve; });
    const send = collectingSend();
    const { state, session, keys } = await establishedChannel(
      store,
      () => new Promise((resolve) => {
        finishRouter = resolve;
        routerStarted();
      }),
      send,
    );
    const envelope = encryptEnvelope({
      key: keys.phoneToMac,
      sessionId: state.session_id,
      sender: 'iphone',
      counter: 0n,
      plaintext: JSON.stringify({
        id: 'rpc_deferred',
        method: 'memex.item.read',
        params: {
          job_id: 'job_00000000000000000000000000000000',
          item_id: 'item_synthetic',
        },
      }),
    });
    const pending = session.receive(JSON.stringify(envelope));
    await started;
    beginFreshPairing(store);
    finishRouter(response);
    await pending;
    assert.equal(send.messages.length, 3);
    assert.equal(JSON.stringify(send.messages).includes('SYNTHETIC_PRIVATE_RESULT_BODY'), false);
    assert.notEqual(session.channel, null);
  }
});

test('successful replacement atomically invalidates the prior trusted channel, not before', async () => {
  const store = new MemoryStateStore();
  let oldRouted = false;
  const oldSend = collectingSend();
  const old = await establishedChannel(
    store,
    async () => { oldRouted = true; return { id: 'rpc_old', result: {} }; },
    oldSend,
  );

  const replacementState = beginFreshPairing(store);
  const replacementIdentity = generateIdentity();
  const replacementEphemeral = generateEphemeral();
  const replacementDeviceId = '66666666-6666-4666-8666-666666666666';
  const replacementSend = collectingSend();
  const replacement = new MacSecureSession({
    stateStore: store,
    router: async () => ({ id: 'rpc_new', result: {} }),
    send: replacementSend,
  });
  await replacement.receive(JSON.stringify(clientHello(
    replacementState,
    replacementIdentity,
    replacementEphemeral,
    replacementDeviceId,
  )));

  const stillTrusted = encryptEnvelope({
    key: old.keys.phoneToMac,
    sessionId: old.state.session_id,
    sender: 'iphone',
    counter: 0n,
    plaintext: JSON.stringify({ id: 'rpc_old', method: 'memex.library.list', params: { cursor: null } }),
  });
  await old.session.receive(JSON.stringify(stillTrusted));
  assert.equal(oldRouted, true);

  const replacementFields = fieldsFromServerHello(replacementSend.messages[0]);
  await replacement.receive(JSON.stringify(clientAuth(
    replacementState,
    replacementIdentity,
    replacementDeviceId,
    replacementFields,
    replacementState.bootstrap_secret,
  )));
  const committed = store.load();
  assert.equal(committed.session_id, replacementState.session_id);
  assert.equal(committed.generation, replacementState.generation);
  assert.equal(committed.pending_pairing, null);
  assert.equal(committed.trusted_phone.phone_device_id, replacementDeviceId);

  const oldAfterReplacement = encryptEnvelope({
    key: old.keys.phoneToMac,
    sessionId: old.state.session_id,
    sender: 'iphone',
    counter: 1n,
    plaintext: JSON.stringify({ id: 'rpc_old_again', method: 'memex.library.list', params: { cursor: null } }),
  });
  await assert.rejects(
    old.session.receive(JSON.stringify(oldAfterReplacement)),
    /untrusted_phone/,
  );
});

test('cross-process pairing rotation linearizes after a locked response queue or suppresses it before', async () => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-response-fence-'));
  const root = fs.realpathSync(rawRoot);
  const store = diskStateStore(root);
  let contenderAttempt;
  const send = collectingSend((_, index) => {
    if (index === 2) contenderAttempt = pairingChild(root).stdout;
  });
  const { state, session, keys } = await establishedChannel(
    store,
    async () => ({
      id: 'rpc_linearized',
      result: { title: 'Synthetic', markdown: 'SYNTHETIC_LINEARIZED_BODY' },
    }),
    send,
  );
  const envelope = encryptEnvelope({
    key: keys.phoneToMac,
    sessionId: state.session_id,
    sender: 'iphone',
    counter: 0n,
    plaintext: JSON.stringify({
      id: 'rpc_linearized',
      method: 'memex.item.read',
      params: {
        job_id: 'job_00000000000000000000000000000000',
        item_id: 'item_synthetic',
      },
    }),
  });
  await session.receive(JSON.stringify(envelope));
  assert.equal(contenderAttempt, 'state_lock_busy');
  assert.equal(send.messages.length, 3);
  const clear = decryptEnvelope({
    key: keys.macToPhone,
    envelope: send.messages[2],
    sessionId: state.session_id,
    sender: 'mac',
    expectedCounter: 1n,
  });
  assert.match(clear.plaintext.toString('utf8'), /SYNTHETIC_LINEARIZED_BODY/);
  assert.equal(pairingChild(root).stdout, 'rotated');
  assert.equal(store.load().session_id, state.session_id);
  assert.notEqual(store.load().pending_pairing.session_id, state.session_id);
  fs.rmSync(root, { recursive: true, force: true });
});

test('dropped ready after durable trust recovers with the exact phone and no retained QR secret', async () => {
  const store = new MemoryStateStore();
  const state = beginFreshPairing(store);
  const phoneIdentity = generateIdentity();
  const phoneDeviceId = '55555555-5555-4555-8555-555555555555';
  const firstEphemeral = generateEphemeral();
  const firstSent = [];
  const firstSession = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: (message) => {
      if (message.kind === 'encrypted_envelope') throw new Error('ready_dropped');
      firstSent.push(message);
    },
  });
  await firstSession.receive(JSON.stringify(clientHello(
    state, phoneIdentity, firstEphemeral, phoneDeviceId,
  )));
  const firstFields = fieldsFromServerHello(firstSent[0]);
  await assert.rejects(firstSession.receive(JSON.stringify(clientAuth(
    state, phoneIdentity, phoneDeviceId, firstFields, state.bootstrap_secret,
  ))), /ready_dropped/);
  assert.equal(firstSession.pending, null);
  assert.deepEqual(store.load().trusted_phone, {
    phone_device_id: phoneDeviceId,
    phone_identity_public_key: phoneIdentity.public_key,
  });
  assert.equal(store.load().pending_pairing, null);

  const recoveryEphemeral = generateEphemeral();
  const recoveredSent = [];
  const recovery = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: (message) => recoveredSent.push(message),
  });
  await recovery.receive(JSON.stringify(clientHello(
    state, phoneIdentity, recoveryEphemeral, phoneDeviceId,
  )));
  const recoveryFields = fieldsFromServerHello(recoveredSent[0]);
  assert.equal(recoveryFields.handshake_mode, 'trusted_recovery');
  const recoveryTranscript = handshakeTranscript(recoveryFields);
  const recoveryKeys = deriveDirectionalKeys(
    recoveryEphemeral.privateKey,
    Buffer.from(recoveryFields.mac_ephemeral_public_key, 'base64url'),
    recoveryTranscript,
    recoveryFields,
  );
  await recovery.receive(JSON.stringify(clientAuth(
    state, phoneIdentity, phoneDeviceId, recoveryFields,
  )));
  const ready = decryptEnvelope({
    key: recoveryKeys.macToPhone,
    envelope: recoveredSent[1],
    sessionId: state.session_id,
    sender: 'mac',
    expectedCounter: 0n,
  });
  assert.equal(JSON.parse(ready.plaintext).type, 'handshake_ready');

  const otherIdentity = generateIdentity();
  const rejected = new MacSecureSession({
    stateStore: store,
    router: async () => assert.fail('router must not run'),
    send: () => {},
  });
  await assert.rejects(rejected.receive(JSON.stringify(clientHello(
    state, otherIdentity, generateEphemeral(), phoneDeviceId,
  ))), /untrusted_phone/);
});
