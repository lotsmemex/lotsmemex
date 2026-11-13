'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const jsQR = require('jsqr');
const QRCode = require('qrcode');

function rasterize(value, scale = 6, margin = 4) {
  const qr = QRCode.create(value, { errorCorrectionLevel: 'M' });
  const modules = qr.modules.size;
  const size = (modules + (margin * 2)) * scale;
  const data = new Uint8ClampedArray(size * size * 4);
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const moduleX = Math.floor(x / scale) - margin;
      const moduleY = Math.floor(y / scale) - margin;
      const dark = moduleX >= 0 && moduleY >= 0
        && moduleX < modules && moduleY < modules
        && qr.modules.get(moduleX, moduleY);
      const offset = ((y * size) + x) * 4;
      const color = dark ? 0 : 255;
      data[offset] = color;
      data[offset + 1] = color;
      data[offset + 2] = color;
      data[offset + 3] = 255;
    }
  }
  return { data, width: size, height: size };
}

test('vendored decoder reads a full non-navigable browser pairing code', () => {
  const code = `MEMEX PAIR 1:${'A'.repeat(768)}`;
  const image = rasterize(code);
  const decoded = jsQR(image.data, image.width, image.height, {
    inversionAttempts: 'attemptBoth',
  });
  assert.equal(decoded.data, code);
  assert.equal(decoded.data.includes('://'), false);
});

test('in-page scanner keeps camera work in the current page and stops every track', async () => {
  const { scanPairingCode } = await import('../web/scanner.mjs');
  const code = `MEMEX PAIR 1:${'B'.repeat(512)}`;
  const image = rasterize(code);
  const tracks = [{ stopped: false, stop() { this.stopped = true; } }];
  const stream = { getTracks: () => tracks };
  const constraints = [];
  const video = {
    muted: false,
    playsInline: false,
    srcObject: null,
    videoWidth: image.width,
    videoHeight: image.height,
    async play() {},
    pause() {},
  };
  const canvas = {
    width: 0,
    height: 0,
    getContext() {
      return {
        drawImage() {},
        getImageData() { return image; },
      };
    },
  };
  let nextFrame = 0;
  const callbacks = new Map();
  const requestFrame = (callback) => {
    const id = ++nextFrame;
    callbacks.set(id, callback);
    queueMicrotask(() => {
      if (callbacks.delete(id)) callback();
    });
    return id;
  };
  const cancelFrame = (id) => callbacks.delete(id);

  const result = await scanPairingCode({
    video,
    canvas,
    mediaDevices: {
      async getUserMedia(value) { constraints.push(value); return stream; },
    },
    decoder: jsQR,
    requestFrame,
    cancelFrame,
    timeoutMs: 5_000,
  });

  assert.equal(result, code);
  assert.deepEqual(constraints, [{
    audio: false,
    video: {
      facingMode: { ideal: 'environment' },
      width: { ideal: 1280 },
      height: { ideal: 720 },
    },
  }]);
  assert.equal(tracks[0].stopped, true);
  assert.equal(video.srcObject, null);
  assert.equal(canvas.width, 0);
  assert.equal(canvas.height, 0);
});

test('scanner fails closed on cancellation and unavailable camera APIs', async () => {
  const { scanPairingCode } = await import('../web/scanner.mjs');
  await assert.rejects(() => scanPairingCode({}), /scanner_unavailable/);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(() => scanPairingCode({
    video: {},
    canvas: {},
    signal: controller.signal,
    mediaDevices: { getUserMedia() {} },
    decoder() {},
    requestFrame() {},
    cancelFrame() {},
  }), /scan_cancelled/);
});
