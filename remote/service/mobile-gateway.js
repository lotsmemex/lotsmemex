'use strict';

// Owner mobile gateway: one loopback process holding the blind relay (which also
// serves the reviewed mobile/web manifest same-origin) and the Mac bridge with
// the scoped library router to the owner daemon socket. Exposed publicly only by
// the owner's named tunnel. Never logs payloads, pairing codes or library data.
//
//   node mobile-gateway.js run    # launchd entry
//   node mobile-gateway.js pair   # forget the phone; the running service shows a new QR
//   node mobile-gateway.js pair --minutes=30  # one extended window; next defaults to 5
//
// Environment:
//   MEMEX_AGENT_SOCKET        owner daemon UDS (required)
//   MEMEX_MOBILE_PUBLIC_URL   https origin with trailing slash (required)
//   MEMEX_MOBILE_ROOT         directory holding the mobile/web manifest (required)
//   MEMEX_MOBILE_STATE_DIR    0700 owner directory for state/admission root (required)
//   MEMEX_MOBILE_PORT         loopback port (required)
//   MEMEX_MOBILE_MAX_SENSITIVITY  phone read ceiling (default private; never restricted)
//   MEMEX_MOBILE_BRAND_NAME, MEMEX_MOBILE_ASSISTANT_NAME  names the phone UI shows
//                              (default Lots MemeX / MemeX)

const fs = require('node:fs');
const path = require('node:path');
const { randomBytes } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { createRelay } = require('../relay/server');
const { log } = require('../relay/diagnostics');
const { BridgeRunner, browserPairingCode } = require('../mac/bridge');
const {
  MacKeychainStateStore, beginFreshPairing, revokeTrust,
} = require('../mac/state-store');
const { SessionGrants, createLibraryRouter } = require('../mac/library-router');
const { presentPairingCode } = require('../mac/pair-presenter');
const { base64url } = require('../protocol');

const MOBILE_FILES = ['index.html', 'app.mjs', 'app.css', 'gateway-source.mjs', 'demo-source.mjs',
  'fixtures.mjs', 'i18n.mjs', 'brand.mjs', 'assets/icon-192.png', 'assets/apple-touch-icon.png',
  'assets/memex-avatar.png', 'changelog.mjs', 'assets/memex-portrait.png', 'zhconv.mjs',
  'assets/opencc/STCharacters.txt', 'assets/opencc/STPhrases.txt',
  'assets/opencc/TSCharacters.txt', 'assets/opencc/TSPhrases.txt', 'assets/opencc/LICENSE-OpenCC.txt',
  'assets/fonts/ChillZhuoKai.woff2', 'assets/fonts/OFL-ChillZhuoKai.txt'];
const APP_PATH = '/mobile/index.html';
const GRANT_MS = 365 * 24 * 60 * 60_000;

function env(name) {
  const value = process.env[name];
  if (!value) throw new Error(`missing_${name.toLowerCase()}`);
  return value;
}

function privateDir(dir) {
  const st = fs.lstatSync(dir);
  if (!st.isDirectory() || st.isSymbolicLink() || st.uid !== process.getuid()
    || (st.mode & 0o777) !== 0o700) throw new Error('unsafe_state_dir');
  return dir;
}

function readPrivateFile(file) {
  let st;
  try { st = fs.lstatSync(file); } catch (e) { if (e.code === 'ENOENT') return null; throw e; }
  if (!st.isFile() || st.isSymbolicLink() || st.uid !== process.getuid() || st.nlink !== 1
    || (st.mode & 0o777) !== 0o600) throw new Error('unsafe_state_file');
  return fs.readFileSync(file);
}

function writePrivateFile(file, body) {
  const tmp = `${file}.tmp-${process.pid}-${randomBytes(6).toString('hex')}`;
  fs.writeFileSync(tmp, body, { flag: 'wx', mode: 0o600 });
  fs.renameSync(tmp, file);
}

function fileStateStore(dir) {
  const file = path.join(dir, 'bridge-state.json');
  return new MacKeychainStateStore({
    lockPath: path.join(dir, 'bridge-state.lock'),
    readState: () => {
      const raw = readPrivateFile(file);
      return raw ? JSON.parse(raw.toString('utf8')) : null;
    },
    writeState: (state) => writePrivateFile(file, JSON.stringify(state)),
  });
}

function loadAdmissionRoot(dir) {
  const file = path.join(dir, 'admission-root');
  const existing = readPrivateFile(file);
  if (existing) return existing.toString('utf8').trim();
  const root = base64url(randomBytes(32));
  writePrivateFile(file, root);
  return root;
}

function takePairingDuration(dir) {
  const file = path.join(dir, 'next-pairing-minutes');
  const raw = readPrivateFile(file);
  if (!raw) return 5 * 60_000;
  const minutes = raw.toString('utf8');
  if (!['5', '30'].includes(minutes)) throw new Error('invalid_pairing_duration');
  fs.unlinkSync(file);
  return Number(minutes) * 60_000;
}

// The product and assistant names, when the owner sets them; otherwise brand.mjs is served as is.
function brandModule({ name, assistant } = {}) {
  if (!name && !assistant) return null;
  for (const value of [name, assistant]) {
    if (value && !/^[^\p{Cc}"'`\\<>]{1,40}$/u.test(value)) throw new Error('invalid_brand_name');
  }
  return Buffer.from(`export const BRAND_NAME = ${JSON.stringify(name || 'Lots MemeX')};\n`
    + `export const ASSISTANT_NAME = ${JSON.stringify(assistant || 'MemeX')};\n`, 'utf8');
}

function mobileRoutes(root, brand = {}) {
  const routes = new Map();
  const brandBody = brandModule(brand);
  for (const file of MOBILE_FILES) {
    const type = file.endsWith('.html') ? 'text/html; charset=utf-8'
      : file.endsWith('.mjs') ? 'text/javascript; charset=utf-8'
        : file.endsWith('.css') ? 'text/css; charset=utf-8'
          : file.endsWith('.txt') ? 'text/plain; charset=utf-8'
            : file.endsWith('.woff2') ? 'font/woff2' : 'image/png';
    routes.set(`/mobile/${file}`, {
      body: file === 'brand.mjs' && brandBody ? brandBody : fs.readFileSync(path.join(root, file)), type,
      headerOptions: { inlineHash: null, modules: true, images: true, connect: true,
        sameOriginFetch: true, fonts: true, camera: file === 'index.html' },
    });
  }
  return routes;
}

function config() {
  const publicUrl = env('MEMEX_MOBILE_PUBLIC_URL');
  const url = new URL(publicUrl);
  if (url.protocol !== 'https:' || url.pathname !== '/' || url.toString() !== publicUrl) {
    throw new Error('invalid_public_url');
  }
  const port = Number.parseInt(env('MEMEX_MOBILE_PORT'), 10);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('invalid_port');
  const maxSensitivity = process.env.MEMEX_MOBILE_MAX_SENSITIVITY || 'private';
  if (!['public', 'internal', 'private'].includes(maxSensitivity)) {
    throw new Error('invalid_max_sensitivity');
  }
  const relayUrl = new URL('/relay', publicUrl).toString().replace(/^https:/, 'wss:');
  return {
    socketPath: env('MEMEX_AGENT_SOCKET'),
    publicUrl, port, maxSensitivity, relayUrl,
    webClientUrl: new URL(APP_PATH, publicUrl).toString(),
    mobileRoot: env('MEMEX_MOBILE_ROOT'),
    brand: { name: process.env.MEMEX_MOBILE_BRAND_NAME, assistant: process.env.MEMEX_MOBILE_ASSISTANT_NAME },
    stateDir: privateDir(env('MEMEX_MOBILE_STATE_DIR')),
  };
}

async function run() {
  const cfg = config();
  const store = fileStateStore(cfg.stateDir);
  const root = loadAdmissionRoot(cfg.stateDir);
  const relay = createRelay({
    host: '127.0.0.1', port: cfg.port, admissionRoot: root, trustProxy: true,
    allowLoopbackMac: true,
    diagnosticSameOrigin: true, diagnosticPublicUrl: cfg.publicUrl,
    application: { path: APP_PATH, routes: mobileRoutes(cfg.mobileRoot, cfg.brand) },
  });
  await relay.listen();
  log(`memex_mobile_relay_ready port=${cfg.port}`);

  const authorization = new SessionGrants();
  const scopes = ['read', 'edit', 'capture'];
  const grantFor = (state) => authorization.set({
    sessionId: state.session_id, generation: state.generation, scopes,
    maxSensitivity: cfg.maxSensitivity, expiresAt: Date.now() + GRANT_MS,
  });
  // Digest of the served mobile/web manifest, shown on the phone's 版本 page.
  const bundle = require('node:crypto').createHash('sha256');
  for (const file of MOBILE_FILES) bundle.update(fs.readFileSync(path.join(cfg.mobileRoot, file)));
  const router = createLibraryRouter({ socketPath: cfg.socketPath, authorization,
    webBundle: bundle.digest('hex').slice(0, 12) });
  const runnerFor = (connectionMode, onPairingConsumed = () => {}) => new BridgeRunner({
    relayUrl: `ws://127.0.0.1:${relay.localMacAddress().port}/relay`,
    socketPath: cfg.socketPath, admissionRoot: root, stateStore: store,
    connectionMode, authorization, router, onPairingConsumed,
  });

  let runner = null;
  let closePairing = null;
  let pairingTimer = null;
  const endPairingWindow = () => {
    clearTimeout(pairingTimer);
    pairingTimer = null;
    try { closePairing?.(); } catch {}
    closePairing = null;
  };

  const state = store.loadOrCreate();
  if (state.trusted_phone) {
    grantFor(state);
    runner = runnerFor('stable');
    runner.start();
    log('memex_mobile_bridge_trusted');
  } else {
    const pending = beginFreshPairing(store, Date.now(), takePairingDuration(cfg.stateDir));
    grantFor(pending);
    runner = runnerFor('pending', () => {
      runner.promotePending();
      endPairingWindow();
      log('memex_mobile_pairing_consumed');
    });
    runner.start();
    const code = browserPairingCode(pending, cfg.relayUrl, root, cfg.webClientUrl);
    closePairing = await presentPairingCode(code, {
      imagePath: path.join(cfg.stateDir, 'pairing-qr.png'),
    });
    // Display and authenticated handshake share the persisted one-time deadline.
    // Run `pair` again for a new one.
    pairingTimer = setTimeout(() => {
      endPairingWindow();
      log('memex_mobile_pairing_expired');
    }, Math.max(0, pending.pairing_expires_at_ms - Date.now()));
    pairingTimer.unref();
    log('memex_mobile_pairing_window_open');
  }

  const shutdown = async () => {
    endPairingWindow();
    runner?.stop();
    await relay.close().catch(() => {});
    process.exit(0);
  };
  process.once('SIGTERM', shutdown);
  process.once('SIGINT', shutdown);
}

function requestFreshPairing(stateDir, minutes = 5) {
  if (![5, 30].includes(minutes)) throw new Error('invalid_pairing_duration');
  revokeTrust(fileStateStore(stateDir));
  writePrivateFile(path.join(stateDir, 'next-pairing-minutes'), String(minutes));
}

function pair() {
  const option = process.argv[3] || '--minutes=5';
  if (!['--minutes=5', '--minutes=30'].includes(option) || process.argv.length > 4) {
    throw new Error('invalid_pairing_duration');
  }
  const cfg = config();
  requestFreshPairing(cfg.stateDir, Number(option.slice(10)));
  log('memex_mobile_trust_revoked');
  // Restart only the exact service label so it opens a fresh pairing window.
  const name = process.env.MEMEX_MOBILE_GATEWAY_LABEL || 'com.lotsmemex.mobile-gateway';
  if (!/^[A-Za-z0-9][A-Za-z0-9.-]{0,127}$/.test(name)) throw new Error('invalid_service_label');
  const label = `gui/${process.getuid()}/${name}`;
  if (spawnSync('/bin/launchctl', ['kickstart', '-k', label], { stdio: 'ignore' }).status !== 0) {
    throw new Error('service_restart_failed');
  }
}

if (require.main === module) {
  const action = process.argv[2];
  Promise.resolve().then(() => {
    if (action === 'run') return run();
    if (action === 'pair') return pair();
    throw new Error('usage_error');
  }).catch(() => {
    log('mobile_gateway_failed', process.stderr);
    process.exit(1);
  });
}

module.exports = { fileStateStore, mobileRoutes, takePairingDuration, requestFreshPairing };
