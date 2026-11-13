'use strict';

const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const jsQR = require('jsqr');
const { PNG } = require('pngjs');
const {
  clearPairingImage,
  openImageWindow,
  presentPairingCode,
  stagePairingCode,
} = require('../mac/pair-presenter');

test('browser pair command has no terminal QR renderer or capability output', () => {
  const source = fs.readFileSync(path.join(__dirname, '../mac/bridge.js'), 'utf8');
  assert.doesNotMatch(source, /QRCode\.toString|type:\s*['"]terminal['"]|process\.stdout.*qrValue/);
  assert.match(source, /process\.stdout\.write\('pairing_window_ready\\n'\)/);
});

test('Quick Look receives only the private image path and no bridge environment', async () => {
  const child = new EventEmitter();
  child.exitCode = null;
  child.signalCode = null;
  child.unref = () => {};
  child.kill = (signal) => { child.signalCode = signal; };
  let invocation;
  const imagePath = '/private/tmp/owner-private-pairing.png';
  const opened = await openImageWindow(imagePath, {
    readinessDelayMs: 0,
    spawnProcess(command, args, options) {
      invocation = { command, args, options };
      setImmediate(() => child.emit('spawn'));
      return child;
    },
  });
  assert.deepEqual(invocation, {
    command: '/usr/bin/qlmanage',
    args: ['-p', imagePath],
    options: { stdio: 'ignore', env: {} },
  });
  assert.equal(JSON.stringify(invocation).includes('MEMEX_RELAY_ADMISSION_ROOT'), false);
  assert.equal(opened.status, 0);
  opened.close();
  assert.equal(child.signalCode, 'SIGTERM');
});

test('Mac pairing opens one ordinary owner-private PNG and removes it on exit', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-pair-window-'));
  const root = fs.realpathSync(rawRoot);
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(rawRoot, { recursive: true, force: true }));
  const imagePath = path.join(root, 'pairing-qr.png');
  const opened = [];
  let closed = 0;
  const pairingCode = `MEMEX PAIR 1:${'A'.repeat(768)}`;
  const cleanup = await presentPairingCode(pairingCode, {
    imagePath,
    openImage(value) {
      opened.push(value);
      return { status: 0, close() { closed += 1; } };
    },
  });

  assert.deepEqual(opened, [imagePath]);
  const metadata = fs.lstatSync(imagePath);
  assert.equal(metadata.isFile(), true);
  assert.equal(metadata.isSymbolicLink(), false);
  assert.equal(metadata.nlink, 1);
  assert.equal(metadata.mode & 0o777, 0o600);
  const image = PNG.sync.read(fs.readFileSync(imagePath));
  assert.equal(image.width, 560);
  assert.equal(image.height, 560);
  assert.equal(jsQR(image.data, image.width, image.height)?.data, pairingCode);

  cleanup();
  cleanup();
  assert.equal(closed, 1);
  assert.equal(fs.existsSync(imagePath), false);
});

test('running bridge stages one private PNG for the native app without launching a helper', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-pair-stage-'));
  const root = fs.realpathSync(rawRoot);
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(rawRoot, { recursive: true, force: true }));
  const imagePath = path.join(root, 'pairing-qr.png');
  const pairingCode = `MEMEX PAIR 1:${'D'.repeat(256)}`;
  const cleanup = await stagePairingCode(pairingCode, { imagePath });
  const metadata = fs.lstatSync(imagePath);
  assert.equal(metadata.isFile(), true);
  assert.equal(metadata.isSymbolicLink(), false);
  assert.equal(metadata.nlink, 1);
  assert.equal(metadata.mode & 0o777, 0o600);
  const image = PNG.sync.read(fs.readFileSync(imagePath));
  assert.equal(image.width, 560);
  assert.equal(image.height, 560);
  assert.equal(jsQR(image.data, image.width, image.height)?.data, pairingCode);
  cleanup();
  assert.equal(fs.existsSync(imagePath), false);

  fs.writeFileSync(imagePath, 'expired', { mode: 0o600 });
  clearPairingImage(imagePath);
  assert.equal(fs.existsSync(imagePath), false);

  const bridgeSource = fs.readFileSync(path.join(__dirname, '../mac/bridge.js'), 'utf8');
  assert.match(bridgeSource, /presentPairingCode:\s*stagePairingCode/);
  assert.match(bridgeSource, /if \(action === 'run'\) \{\s*clearPairingImage\(\)/);
});

test('late cleanup cannot remove a replacement pairing image', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-pair-replacement-'));
  const root = fs.realpathSync(rawRoot);
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(rawRoot, { recursive: true, force: true }));
  const imagePath = path.join(root, 'pairing-qr.png');
  const cleanup = await stagePairingCode(`MEMEX1:${'a'.repeat(48)}`, { imagePath });
  fs.unlinkSync(imagePath);
  fs.writeFileSync(imagePath, Buffer.from('replacement'), { flag: 'wx', mode: 0o600 });
  const replacement = fs.lstatSync(imagePath);
  cleanup();
  const survived = fs.lstatSync(imagePath);
  assert.equal(survived.dev, replacement.dev);
  assert.equal(survived.ino, replacement.ino);
});

test('Mac presenter fails closed on unsafe storage or failed private window open', async (t) => {
  const rawRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-pair-window-'));
  const root = fs.realpathSync(rawRoot);
  t.after(() => fs.rmSync(rawRoot, { recursive: true, force: true }));
  fs.chmodSync(root, 0o755);
  await assert.rejects(() => presentPairingCode(`MEMEX PAIR 1:${'B'.repeat(64)}`, {
    imagePath: path.join(root, 'unsafe.png'),
    openImage: () => ({ status: 0 }),
  }), /unsafe_pairing_directory/);

  fs.chmodSync(root, 0o700);
  const imagePath = path.join(root, 'failed.png');
  await assert.rejects(() => presentPairingCode(`MEMEX PAIR 1:${'C'.repeat(64)}`, {
    imagePath,
    openImage: () => ({ status: 1 }),
  }), /pairing_window_unavailable/);
  assert.equal(fs.existsSync(imagePath), false);

  const realParent = path.join(root, 'real-parent');
  const linkedParent = path.join(root, 'linked-parent');
  const realChild = path.join(realParent, 'child');
  fs.mkdirSync(realChild, { recursive: true, mode: 0o700 });
  fs.symlinkSync(realParent, linkedParent);
  await assert.rejects(() => stagePairingCode(`MEMEX PAIR 1:${'E'.repeat(64)}`, {
    imagePath: path.join(linkedParent, 'child', 'pairing.png'),
  }), /unsafe_pairing_directory/);
  assert.equal(fs.existsSync(path.join(realChild, 'pairing.png')), false);
});
