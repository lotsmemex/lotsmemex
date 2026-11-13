'use strict';

// Test supply, not an installed gateway: creates its own synthetic Store and
// loopback relay. No library/socket/config argument or Keychain access exists.
const { spawn, execFile } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs');
const path = require('node:path');
const { randomBytes, createHash } = require('node:crypto');
const { promisify } = require('node:util');
const { setTimeout: delay } = require('node:timers/promises');
const { createRelay } = require('../relay/server');
const { BridgeRunner, browserPairingCode } = require('../mac/bridge');
const { MemoryStateStore, beginFreshPairing, pendingPairingState } = require('../mac/state-store');
const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
const { sendOwnerCommand } = require('../mac/ipc-client');
const { mobileRoutes } = require('../service/mobile-gateway');

// Optional frontend supply: a real additional public Store item, with a test-only
// paired-edition response adapter. This is not a paired-book import/daemon proof.
// The adapter still reads the real item on every request (permissions/trash/CAS),
// never claims a daemon capability, and is never used by the installed gateway.
async function seedReaderBook(cwd, root) {
  const sha256 = (body) => createHash('sha256').update(body).digest('hex');
  const parts = {
    original: [
      ['chapter-1', 'First light', '# First light\n\nA synthetic reader follows the morning light. 🙂\n\n'],
      ['chapter-2', 'The bridge', '# The bridge\n\nTwo editions meet at a chapter, not at a byte offset.\n\n'],
      ['appendix', 'Original-only note', '# Original-only note\n\nThis appendix has no translated counterpart.\n'],
    ],
    translation: [
      ['chapter-1', '晨光', '# 晨光\n\n合成读者沿着晨光前行。🙂\n\n'],
      ['chapter-2', '桥梁', '# 桥梁\n\n两种版本在章节相遇，不在字节偏移处对齐。\n'],
    ],
  };
  const bodies = Object.fromEntries(Object.entries(parts).map(([side, chapters]) =>
    [side, Buffer.from(chapters.map((c) => c[2]).join(''))]));
  const editions = Object.entries(parts).map(([side, chapters]) => {
    let end = 0;
    return { side, language: side === 'original' ? 'en' : 'zh',
      sha256: sha256(bodies[side]), bytes: bodies[side].length,
      chapters: chapters.map(([key, title, text]) => {
        const start = end;
        end += Buffer.byteLength(text);
        return { key, title, level: 1, start, end, counterpart: key === 'appendix' ? null : key };
      }) };
  });
  const dir = path.join(root, 'reader-fixture');
  fs.mkdirSync(dir);
  fs.writeFileSync(path.join(dir, 'translation.md'), bodies.translation);
  fs.writeFileSync(path.join(dir, 'source.json'), JSON.stringify({ schema_version: 1,
    source_id: 'synthetic-reader', name: 'Synthetic bilingual book', source_kind: 'web',
    default_sensitivity: 'public', preferred_translator: null }));
  fs.writeFileSync(path.join(dir, 'capture.json'), JSON.stringify({ schema_version: 1,
    adapter_id: 'mobile-reader-fixture', adapter_version: '1', external_capture_id: 'synthetic-reader-v1',
    source_id: 'synthetic-reader', captured_at: '2026-01-01T00:00:00Z',
    capture_method: 'browser_snapshot_v1', sensitivity: 'public',
    title: 'Bilingual reader fixture / 双语阅读', source_url: 'https://synthetic.example.invalid/reader-book',
    artifacts: [{ schema_version: 1, locator: 'translation.md', media_type: 'text/markdown',
      byte_size: bodies.translation.length, sha256: sha256(bodies.translation) }] }));
  const { stdout } = await promisify(execFile)(path.join(cwd, 'target/debug/memex'), [
    'ingest', '--source', path.join(dir, 'source.json'), '--capture', path.join(dir, 'capture.json'),
    '--library', path.join(root, 'Library'),
  ], { cwd, timeout: 15_000 });
  const ingest = JSON.parse(stdout);
  const document = { item_id: ingest.item_ids[0], revision_id: ingest.revision_ids[0],
    canonical_sha256: sha256(bodies.translation), default_side: 'translation',
    alignment: 'declared_chapter_order_not_paragraph_alignment', editions };
  return { document, async read(socket, command, send) {
    const p = command.request;
    const current = await send(socket, { command: 'scoped_item_read', item_id: document.item_id,
      max_sensitivity: command.max_sensitivity });
    if (!current.ok) return current;
    const fail = (reason_code) => ({ ok: false, reason_code });
    if (p.revision_id !== current.library_entry.revision_id) return fail('stale_item_revision');
    if (current.library_entry.content_sha256 !== `sha256:${document.canonical_sha256}`) return fail('stale_book_manifest');
    const identity = { item_id: document.item_id, revision_id: p.revision_id };
    if (p.op === 'toc') return { ok: true, reader: { ...document, ...identity } };
    if (p.op === 'alignment') {
      const original = editions.find((e) => e.side === 'original').chapters.find((c) => c.key === p.chapter);
      if (!original) return fail('reader_chapter_not_found');
      if (p.expected_version !== 'any' && p.expected_version !== 'missing') return fail('stale_chapter_alignment');
      if (p.offset !== 0) return fail('invalid_reader_alignment_request');
      const translation = editions.find((e) => e.side === 'translation').chapters.find((c) => c.key === original.counterpart);
      return { ok: true, reader: { schema_version: 1, ...identity, version: 'missing',
        original_chapter: original.key, translation_chapter: translation?.key ?? null,
        original_sha256: sha256(bodies.original.subarray(original.start, original.end)),
        translation_sha256: translation ? sha256(bodies.translation.subarray(translation.start, translation.end)) : null,
        original_bytes: original.end - original.start,
        translation_bytes: translation ? translation.end - translation.start : null,
        status: 'none', quality: 'not_certified', offset: 0, total: 0, next_offset: null, pairs: [] } };
    }
    if (p.op !== 'chunk') return fail('invalid_reader_request');
    const edition = editions.find((e) => e.side === p.side);
    if (!edition) return fail('reader_side_not_found');
    const chapter = edition.chapters.find((c) => c.key === p.chapter);
    if (!chapter) return fail('reader_chapter_not_found');
    const body = bodies[p.side];
    const start = chapter.start + p.offset;
    const boundary = (n) => n === body.length || (body[n] & 0xc0) !== 0x80;
    if (start > chapter.end || !boundary(start)) return fail('invalid_reader_offset');
    let end = Math.min(start + p.limit, chapter.end);
    while (!boundary(end)) end -= 1;
    return { ok: true, reader: { ...identity, side: p.side, sha256: edition.sha256,
      evidence_id: `evidence_synthetic_reader_${p.side}`, chapter: p.chapter, start, end,
      next_offset: end < chapter.end ? end - chapter.start : null,
      markdown: body.subarray(start, end).toString('utf8') } };
  } };
}

function readReceipt(child) {
  return new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => reject(new Error('fixture_start_timeout')), 15_000);
    child.once('error', (e) => { clearTimeout(timer); reject(e); });
    child.once('exit', () => { clearTimeout(timer); reject(new Error('fixture_start_failed')); });
    child.stderr.resume();
    child.stdout.on('data', (chunk) => {
      output += chunk;
      if (!output.includes('\n')) return;
      clearTimeout(timer);
      try { resolve(JSON.parse(output.split('\n')[0])); } catch (e) { reject(e); }
    });
  });
}

async function startMobileFixture({ now = () => Date.now(), sendCommand, mobileRoot, capture = false,
  seedAi = false, seedImages = false, seedReader = false, seedParagraphs = false,
  seedRepair = false, seedFragmented = false, repairOutcome = 'success',
  seedTranslation = false, translationProjection = null, translationOutcome = 'success', seedFormat = false, formatOutcome = 'success' } = {}) {
  const cwd = path.resolve(__dirname, '../..');
  const spawnDaemon = (args) => spawn(path.join(cwd, 'target/debug/examples/mobile_fixture'), args, {
    cwd, env: { ...process.env, TMPDIR: '/private/tmp' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  // `seedAi`: one more public essay with seeded synthetic insights and chat history.
  if (!['success', 'failed', 'unknown'].includes(repairOutcome)) throw new Error('invalid_fixture');
  const repairArgs = repairOutcome === 'failed' ? ['--repair-fail'] : repairOutcome === 'unknown' ? ['--repair-unknown'] : [];
  if (seedFragmented) repairArgs.push('--seed-fragmented');
  if (!['success', 'failed', 'unknown'].includes(translationOutcome)) throw new Error('invalid_fixture');
  if (seedTranslation) repairArgs.push('--seed-translation');
  if (![null, 'legacy', 'mapped'].includes(translationProjection)) throw new Error('invalid_fixture');
  if (translationProjection) repairArgs.push(`--seed-translation-${translationProjection}`);
  if (translationOutcome === 'failed') repairArgs.push('--translation-fail');
  if (translationOutcome === 'unknown') repairArgs.push('--translation-unknown');
  if (!['success', 'failed', 'unknown'].includes(formatOutcome)) throw new Error('invalid_fixture');
  if (seedFormat) repairArgs.push('--seed-format');
  if (formatOutcome === 'failed') repairArgs.push('--format-fail');
  if (formatOutcome === 'unknown') repairArgs.push('--format-unknown');
  let child = spawnDaemon([...(capture ? ['--capture'] : []), ...(seedAi ? ['--seed-ai'] : []), ...(seedImages ? ['--seed-images'] : []), ...(seedParagraphs ? ['--seed-paragraphs'] : []), ...(seedRepair ? ['--seed-repair'] : []), ...repairArgs]);
  let receipt;
  let relay;
  let runner;
  let closed = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    runner?.stop();
    if (relay) await relay.close();
    if (child.pid && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, 'exit');
      child.kill('SIGTERM');
      await exited;
    }
    if (receipt?.root && path.dirname(receipt.root) === '/private/tmp'
      && path.basename(receipt.root).startsWith('memex-mobile-')) {
      fs.rmSync(receipt.root, { recursive: true, force: true });
    }
  };
  try {
    receipt = await readReceipt(child);
    if (!receipt.synthetic_only) throw new Error('invalid_fixture');
    for (let i = 0; !fs.existsSync(receipt.socket); i += 1) {
      if (i === 100 || child.exitCode !== null) throw new Error('fixture_socket_unavailable');
      await delay(25);
    }
    const reader = seedReader ? await seedReaderBook(cwd, receipt.root) : null;
    const ownerSend = sendCommand || sendOwnerCommand;
    const fixtureSend = async (socket, command, options) => {
      if (reader && command.command === 'reader' && command.request.item_id === reader.document.item_id) {
        return reader.read(socket, command, ownerSend);
      }
      const response = await ownerSend(socket, command, options);
      // This legacy adapter has no persisted book manifest or repair jobs.
      // Actual repair-capable integration uses seedRepair, never this adapter.
      if (reader && command.command === 'capabilities' && response.capabilities) {
        return { ...response, capabilities: { ...response.capabilities,
          features: response.capabilities.features.filter((f) => !['reader_chapter_repair_v1', 'reader_chapter_edit_v1'].includes(f)) } };
      }
      return response;
    };
    const root = randomBytes(32);
    let browserOrigin = 'https://mobile-fixture.example';
    // Exercise the production whitelist and MIME contract, not a drifting copy.
    const routes = mobileRoot ? mobileRoutes(mobileRoot) : new Map();
    relay = createRelay({ host: '127.0.0.1', port: 0, admissionRoot: root, allowLoopbackMac: true,
      ...(mobileRoot ? { diagnosticSameOrigin: true, diagnosticPublicUrl: 'http://127.0.0.1:0/',
        application: { path: '/mobile/index.html', routes } } : { browserOrigin }),
    });
    const address = await relay.listen();
    const relayUrl = `ws://127.0.0.1:${address.port}/relay`;
    const bridgeRelayUrl = `ws://127.0.0.1:${relay.localMacAddress().port}/relay`;
    if (mobileRoot) browserOrigin = `http://127.0.0.1:${address.port}`;
    const webClientUrl = `${browserOrigin}${mobileRoot ? '/mobile/index.html' : '/mobile'}`;
    const stateStore = new MemoryStateStore();
    beginFreshPairing(stateStore);
    let state = pendingPairingState(stateStore.load());
    const authorization = new SessionGrants({ now });
    let grant = {
      sessionId: state.session_id, generation: state.generation,
      scopes: capture ? ['read', 'capture', 'edit'] : ['read', 'edit'],
      maxSensitivity: capture ? 'internal' : 'public', expiresAt: now() + 60 * 60_000,
    };
    authorization.set(grant);
    runner = new BridgeRunner({
      relayUrl: bridgeRelayUrl, socketPath: receipt.socket, admissionRoot: root, stateStore,
      connectionMode: 'pending', authorization,
      router: createLibraryRouter({ socketPath: receipt.socket, authorization, sendCommand: fixtureSend, webBundle: mobileRoot ? 'fixturebundle' : null }),
      onPairingConsumed: () => runner.promotePending(),
    });
    runner.start();
    await once(runner.socket, 'open');
    const fixture = {
      ...receipt, relayUrl, webClientUrl, browserOrigin, stateStore, authorization, grant,
      bridgeRelayUrl,
      readerBook: receipt.paragraphBook ?? reader?.document ?? null,
      pairingCode: browserPairingCode(state, relayUrl, root, webClientUrl, { allowLocal: true }), close,
      async restartBridge() {
        const socket = runner.socket;
        const closed = socket && socket.readyState !== 3 ? once(socket, 'close') : Promise.resolve();
        runner.stop();
        await closed;
        runner.start();
        await once(runner.socket, 'open');
      },
      async restartDaemon() {
        const exited = once(child, 'exit');
        child.kill('SIGTERM');
        await exited;
        child = spawnDaemon(['--resume', receipt.root, ...(capture ? ['--capture'] : []), ...repairArgs]);
        const resumed = await readReceipt(child);
        if (resumed.root !== receipt.root) throw new Error('invalid_fixture');
        fixture.pid = resumed.pid;
        for (let i = 0; i < 100; i += 1) {
          try {
            const response = await sendOwnerCommand(receipt.socket, { command: 'list', cursor: null });
            if (response.ok) return;
          } catch { /* the owned socket is being rebound */ }
          await delay(25);
        }
        throw new Error('fixture_restart_failed');
      },
      async rePair() {
        runner.stop();
        authorization.revoke(grant.sessionId);
        beginFreshPairing(stateStore);
        state = pendingPairingState(stateStore.load());
        grant = { ...grant, sessionId: state.session_id, generation: state.generation };
        authorization.set(grant);
        runner = new BridgeRunner({
          relayUrl: bridgeRelayUrl, socketPath: receipt.socket, admissionRoot: root, stateStore,
          connectionMode: 'pending', authorization,
          router: createLibraryRouter({ socketPath: receipt.socket, authorization, sendCommand: fixtureSend, webBundle: mobileRoot ? 'fixturebundle' : null }),
          onPairingConsumed: () => runner.promotePending(),
        });
        runner.start();
        await once(runner.socket, 'open');
        fixture.grant = grant;
        fixture.pairingCode = browserPairingCode(state, relayUrl, root, webClientUrl, { allowLocal: true });
      },
    };
    return fixture;
  } catch (e) {
    await close();
    throw e;
  }
}

module.exports = { startMobileFixture };

if (require.main === module) {
  startMobileFixture({ mobileRoot: process.argv.slice(2).find((arg) => !arg.startsWith('--')),
    seedReader: process.argv.includes('--seed-reader'), seedParagraphs: process.argv.includes('--seed-paragraphs'),
    seedRepair: process.argv.includes('--seed-repair'),
    seedFragmented: process.argv.includes('--seed-fragmented'),
    seedTranslation: process.argv.includes('--seed-translation'),
    translationProjection: process.argv.includes('--seed-translation-legacy') ? 'legacy' : process.argv.includes('--seed-translation-mapped') ? 'mapped' : null,
    seedFormat: process.argv.includes('--seed-format'),
    formatOutcome: process.argv.includes('--format-fail') ? 'failed' : process.argv.includes('--format-unknown') ? 'unknown' : 'success',
    translationOutcome: process.argv.includes('--translation-fail') ? 'failed' : process.argv.includes('--translation-unknown') ? 'unknown' : 'success',
    repairOutcome: process.argv.includes('--repair-fail') ? 'failed' : process.argv.includes('--repair-unknown') ? 'unknown' : 'success' }).then((fixture) => {
    // Ephemeral synthetic pairing only; never publish this local receipt.
    console.log(JSON.stringify({ synthetic_only: true, pid: process.pid,
      daemon_pid: fixture.pid, root: fixture.root, relayUrl: fixture.relayUrl,
      webClientUrl: fixture.webClientUrl, browserOrigin: fixture.browserOrigin,
      readerBook: fixture.readerBook,
      pairingCode: fixture.pairingCode }));
    process.once('SIGTERM', () => fixture.close().then(() => process.exit(0)));
    process.once('SIGINT', () => fixture.close().then(() => process.exit(0)));
  }).catch(() => { console.error('mobile_fixture_failed'); process.exitCode = 1; });
}
