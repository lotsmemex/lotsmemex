const MAX_PAIRING_CODE_LENGTH = 4_096;
const MAX_FRAME_WIDTH = 1_600;
const MAX_FRAME_HEIGHT = 1_200;
const DEFAULT_TIMEOUT_MS = 2 * 60_000;
const SCAN_INTERVAL_MS = 120;

function stopStream(stream) {
  if (!stream || typeof stream.getTracks !== 'function') return;
  for (const track of stream.getTracks()) {
    try { track.stop(); } catch { /* already stopped */ }
  }
}

function decodedValue(result) {
  const value = result?.data;
  if (
    typeof value !== 'string'
    || value.length < 1
    || value.length > MAX_PAIRING_CODE_LENGTH
    || /[\r\n\x00-\x1f\x7f]/.test(value)
  ) return null;
  return value;
}

export async function scanPairingCode({
  video,
  canvas,
  signal,
  mediaDevices = globalThis.navigator?.mediaDevices,
  decoder = globalThis.jsQR,
  requestFrame = globalThis.requestAnimationFrame?.bind(globalThis),
  cancelFrame = globalThis.cancelAnimationFrame?.bind(globalThis),
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  if (
    !video || !canvas
    || !mediaDevices || typeof mediaDevices.getUserMedia !== 'function'
    || typeof decoder !== 'function'
    || typeof requestFrame !== 'function'
    || typeof cancelFrame !== 'function'
    || !Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 5 * 60_000
  ) throw new Error('scanner_unavailable');
  if (signal?.aborted) throw new Error('scan_cancelled');

  let stream;
  let frame = null;
  let timer = null;
  let abortListener;
  try {
    stream = await mediaDevices.getUserMedia({
      audio: false,
      video: {
        facingMode: { ideal: 'environment' },
        width: { ideal: 1280 },
        height: { ideal: 720 },
      },
    });
    if (signal?.aborted) throw new Error('scan_cancelled');
    video.srcObject = stream;
    video.muted = true;
    video.playsInline = true;
    await video.play();

    return await new Promise((resolve, reject) => {
      let settled = false;
      let lastScanAt = 0;
      const finish = (error, value) => {
        if (settled) return;
        settled = true;
        if (frame !== null) cancelFrame(frame);
        if (timer !== null) clearTimeout(timer);
        if (signal && abortListener) signal.removeEventListener('abort', abortListener);
        if (error) reject(error); else resolve(value);
      };
      abortListener = () => finish(new Error('scan_cancelled'));
      signal?.addEventListener('abort', abortListener, { once: true });
      timer = setTimeout(() => finish(new Error('scan_timeout')), timeoutMs);

      const scan = () => {
        if (settled) return;
        const current = Date.now();
        if (current - lastScanAt < SCAN_INTERVAL_MS) {
          frame = requestFrame(scan);
          return;
        }
        lastScanAt = current;
        const sourceWidth = Number(video.videoWidth);
        const sourceHeight = Number(video.videoHeight);
        if (sourceWidth > 0 && sourceHeight > 0) {
          const scale = Math.min(
            1,
            MAX_FRAME_WIDTH / sourceWidth,
            MAX_FRAME_HEIGHT / sourceHeight,
          );
          const width = Math.max(1, Math.floor(sourceWidth * scale));
          const height = Math.max(1, Math.floor(sourceHeight * scale));
          if (canvas.width !== width) canvas.width = width;
          if (canvas.height !== height) canvas.height = height;
          const context = canvas.getContext('2d', { willReadFrequently: true });
          if (!context) {
            finish(new Error('scanner_unavailable'));
            return;
          }
          try {
            context.drawImage(video, 0, 0, width, height);
            const image = context.getImageData(0, 0, width, height);
            const value = decodedValue(decoder(image.data, width, height, {
              inversionAttempts: 'attemptBoth',
            }));
            if (value) {
              finish(null, value);
              return;
            }
          } catch { /* a not-yet-ready Safari frame is retried */ }
        }
        frame = requestFrame(scan);
      };
      frame = requestFrame(scan);
    });
  } catch (error) {
    if (error?.message === 'scan_cancelled' || error?.message === 'scan_timeout'
      || error?.message === 'scanner_unavailable') throw error;
    throw new Error('camera_unavailable');
  } finally {
    if (frame !== null) cancelFrame(frame);
    if (timer !== null) clearTimeout(timer);
    stopStream(stream);
    try { video.pause(); } catch { /* unavailable */ }
    video.srcObject = null;
    canvas.width = 0;
    canvas.height = 0;
  }
}
