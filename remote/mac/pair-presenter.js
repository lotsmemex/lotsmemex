'use strict';

const { dataDir } = require('../data-dir');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');
const QRCode = require('qrcode');

const DEFAULT_IMAGE_PATH = path.join(dataDir(), 'remote-managed/pairing-qr.png');

function assertPairingCode(value) {
  if (
    typeof value !== 'string'
    || value.length < 32
    || value.length > 4_096
    || !/^(?:MEMEX PAIR 1:|MEMEX1:)[A-Za-z0-9_-]+$/.test(value)
  ) throw new Error('invalid_pairing_code');
}

function assertPrivateDirectory(directory) {
  const metadata = fs.lstatSync(directory);
  if (
    !metadata.isDirectory()
    || metadata.isSymbolicLink()
    || metadata.uid !== process.getuid()
    || (metadata.mode & 0o777) !== 0o700
    || fs.realpathSync(directory) !== path.resolve(directory)
  ) throw new Error('unsafe_pairing_directory');
  return metadata;
}

function sameFile(left, right) {
  return left.dev === right.dev && left.ino === right.ino;
}

function removeImage(imagePath) {
  try {
    const metadata = fs.lstatSync(imagePath);
    if (
      !metadata.isFile()
      || metadata.isSymbolicLink()
      || metadata.uid !== process.getuid()
      || metadata.nlink !== 1
    ) throw new Error('unsafe_pairing_image');
    fs.unlinkSync(imagePath);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

function removeExactImage(imagePath, descriptor) {
  try {
    const expected = fs.fstatSync(descriptor);
    const metadata = fs.lstatSync(imagePath);
    if (
      !expected.isFile()
      || expected.uid !== process.getuid()
      || expected.nlink !== 1
      || !metadata.isFile()
      || metadata.isSymbolicLink()
      || metadata.uid !== process.getuid()
      || metadata.nlink !== 1
    ) throw new Error('unsafe_pairing_image');
    if (sameFile(metadata, expected)) fs.unlinkSync(imagePath);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

function clearPairingImage(imagePath = DEFAULT_IMAGE_PATH) {
  if (typeof imagePath !== 'string' || !path.isAbsolute(imagePath)) {
    throw new Error('unsafe_pairing_image');
  }
  assertPrivateDirectory(path.dirname(imagePath));
  removeImage(imagePath);
}

function openImageWindow(imagePath, {
  spawnProcess = spawn,
  readinessDelayMs = 250,
} = {}) {
  return new Promise((resolve) => {
    let settled = false;
    let readinessTimer;
    const child = spawnProcess('/usr/bin/qlmanage', ['-p', imagePath], {
      stdio: 'ignore',
      env: {},
    });
    child.unref();
    const finish = (status) => {
      if (settled) return;
      settled = true;
      clearTimeout(readinessTimer);
      resolve({
        status,
        close() {
          if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
        },
      });
    };
    child.once('error', () => finish(1));
    child.once('exit', (status) => finish(status === 0 ? 1 : status ?? 1));
    child.once('spawn', () => {
      readinessTimer = setTimeout(() => finish(0), readinessDelayMs);
    });
  });
}

async function presentPairingCode(code, {
  imagePath = DEFAULT_IMAGE_PATH,
  openImage = openImageWindow,
} = {}) {
  const removePairingImage = await stagePairingCode(code, { imagePath });
  const opened = await openImage(imagePath);
  if (!opened || opened.status !== 0) {
    removePairingImage();
    throw new Error('pairing_window_unavailable');
  }
  let removed = false;
  return () => {
    if (removed) return;
    removed = true;
    try { opened.close?.(); } catch {}
    removePairingImage();
  };
}

async function stagePairingCode(code, {
  imagePath = DEFAULT_IMAGE_PATH,
} = {}) {
  assertPairingCode(code);
  if (typeof imagePath !== 'string' || !path.isAbsolute(imagePath)) {
    throw new Error('unsafe_pairing_image');
  }
  const directory = path.dirname(imagePath);
  const initialParent = assertPrivateDirectory(directory);
  removeImage(imagePath);
  const png = await QRCode.toBuffer(code, {
    type: 'png', errorCorrectionLevel: 'M', margin: 4, width: 560,
  });
  if (!sameFile(initialParent, assertPrivateDirectory(directory))) {
    throw new Error('unsafe_pairing_directory');
  }
  fs.writeFileSync(imagePath, png, { flag: 'wx', mode: 0o600 });
  const metadata = fs.lstatSync(imagePath);
  if (
    !metadata.isFile()
    || metadata.isSymbolicLink()
    || metadata.uid !== process.getuid()
    || metadata.nlink !== 1
    || (metadata.mode & 0o777) !== 0o600
    || !sameFile(initialParent, assertPrivateDirectory(directory))
  ) {
    try { removeImage(imagePath); } catch {}
    throw new Error('unsafe_pairing_image');
  }
  let descriptor;
  try {
    descriptor = fs.openSync(imagePath, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    const opened = fs.fstatSync(descriptor);
    if (
      !opened.isFile()
      || opened.uid !== process.getuid()
      || opened.nlink !== 1
      || (opened.mode & 0o777) !== 0o600
      || !sameFile(metadata, opened)
    ) throw new Error('unsafe_pairing_image');
  } catch {
    if (descriptor !== undefined) fs.closeSync(descriptor);
    try { removeImage(imagePath); } catch {}
    throw new Error('unsafe_pairing_image');
  }
  let removed = false;
  return () => {
    if (removed) return;
    removed = true;
    try { removeExactImage(imagePath, descriptor); } catch {}
    finally { fs.closeSync(descriptor); }
  };
}

module.exports = {
  clearPairingImage,
  DEFAULT_IMAGE_PATH,
  openImageWindow,
  presentPairingCode,
  stagePairingCode,
};
