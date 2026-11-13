'use strict';

const assert = require('node:assert/strict');
const { createHash, randomBytes } = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const test = require('node:test');
const { createRelay } = require('../relay/server');

const DIAGNOSTIC_PUBLIC_URL = 'https://review.example/';
const DIAGNOSTIC_RELAY_URL = 'wss://review.example/relay';

function diagnosticRelay(options = {}) {
  return createRelay({
    port: 0,
    admissionRoot: randomBytes(32),
    diagnosticPublicUrl: DIAGNOSTIC_PUBLIC_URL,
    diagnosticSameOrigin: true,
    ...options,
  });
}

function request(port, path, method = 'GET') {
  return new Promise((resolve, reject) => {
    const req = http.request({
      host: '127.0.0.1', port, path, method, agent: false, headers: { connection: 'close' },
    }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolve({
        status: response.statusCode,
        headers: response.headers,
        body: Buffer.concat(chunks),
      }));
    });
    req.on('error', reject);
    req.end();
  });
}

function inlineBootstrap(html, { hash = '', search = '' } = {}) {
  const script = html.match(/<script>([\s\S]*?)<\/script>/)?.[1];
  assert.ok(script);
  const events = [];
  const starts = [];
  const elements = new Map();
  const location = {
    origin: 'https://review.example',
    pathname: '/',
    hash,
    search,
    get href() { return `${this.origin}${this.pathname}${this.search}${this.hash}`; },
  };
  const documentValue = {
    getElementById(id) {
      if (!elements.has(id)) {
        const listeners = new Map();
        elements.set(id, {
          disabled: false,
          hidden: true,
          textContent: '',
          addEventListener(name, listener) { listeners.set(name, listener); },
          activate(name = 'click') { listeners.get(name)?.(); },
        });
      }
      return elements.get(id);
    },
  };
  const windowValue = {
    location,
    history: {
      replaceState(_state, _title, pathname) {
        events.push('clear');
        location.pathname = pathname;
        location.hash = '';
        location.search = '';
      },
    },
  };
  const loadModule = (specifier) => {
    events.push(`import:${specifier}`);
    return Promise.resolve({
      start(options) {
        events.push('start');
        starts.push(options);
      },
      startLibrary(options) {
        events.push('startLibrary');
        starts.push(options);
      },
    });
  };
  const executable = script.replace(
    "import('/remote/v1/app.mjs')",
    "loadModule('/remote/v1/app.mjs')",
  );
  assert.notEqual(executable, script);
  Function('window', 'document', 'loadModule', `'use strict';${executable}`)(
    windowValue, documentValue, loadModule,
  );
  return {
    elements,
    events,
    location,
    starts,
    settle: async () => {
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

test('relay serves no-store root and exact client assets with injection-resistant headers', async (t) => {
  const relay = diagnosticRelay();
  t.after(() => relay.close());
  const { port } = await relay.listen();
  for (const path of [
    '/', '/settings', '/account', '/remote/v1/', '/remote/v1/app.mjs', '/remote/v1/protocol.mjs',
    '/remote/v1/storage.mjs', '/remote/v1/scanner.mjs', '/remote/v1/jsqr.js',
    '/remote/v1/app.css', '/remote/v1/config.mjs',
  ]) {
    const response = await request(port, path);
    assert.equal(response.status, 200, path);
    assert.equal(response.headers['x-amp-review-widget'], 'off', path);
    assert.equal(response.headers['referrer-policy'], 'no-referrer', path);
    assert.equal(response.headers['x-content-type-options'], 'nosniff', path);
    assert.equal(response.headers['cross-origin-resource-policy'], 'same-origin', path);
    assert.equal(response.headers['cache-control'], 'no-store, no-transform', path);
    assert.equal(response.headers['content-length'], String(response.body.length), path);
  }

  const root = await request(port, '/');
  const html = root.body.toString('utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/)?.[1];
  assert.ok(script);
  const hash = createHash('sha256').update(script).digest('base64');
  assert.match(root.headers['content-security-policy'], new RegExp(`'sha256-${hash.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}'`));
  assert.match(root.headers['content-security-policy'], /default-src 'none'/);
  assert.match(root.headers['content-security-policy'], /worker-src 'none'/);
  assert.match(root.headers['content-security-policy'], /connect-src wss:\/\/review\.example/);
  assert.match(root.headers['content-security-policy'], /font-src 'none'/);
  assert.match(root.headers['content-security-policy'], /media-src 'none'/);
  assert.match(root.headers['permissions-policy'], /camera=\(\)/);
  assert.match(root.headers['permissions-policy'], /microphone=\(\)/);
  assert.doesNotMatch(root.headers['content-security-policy'], /unsafe-inline|unsafe-eval/);
  assert.doesNotMatch(html, /(?:src|href)=["']https?:\/\//);
  assert.doesNotMatch(html, /serviceWorker|localStorage|analytics/i);
  assert.match(html, /<html lang="zh-CN">/);
  assert.match(html, /Lots MemeX 资料库/);
  assert.match(html, /id="library"/);
  assert.match(html, /id="library-search"[\s\S]*id="library-filter"/);
  assert.match(html, /id="library-count"/);
  assert.match(html, /Mac Store 当前不可用，未显示权威条目数/);
  assert.match(html, /href="\/settings"/);
  assert.match(html, /href="\/account"/);
  assert.doesNotMatch(html, /scan-pairing-button|pairing-video|capture-form|jsqr\.js/);
  assert.doesNotMatch(html, /Private capture|Capture a public page|Refresh status|Stop checking locally/);

  const settings = await request(port, '/settings');
  const settingsHtml = settings.body.toString('utf8');
  assert.match(settings.headers['content-security-policy'], /media-src 'self' blob:/);
  assert.match(settings.headers['permissions-policy'], /camera=\(self\)/);
  assert.match(settingsHtml, /href="\/">← 返回资料库/);
  assert.match(settingsHtml, /<label for="capture-url">公开 HTTPS 网址<\/label>/);
  assert.match(settingsHtml, /<textarea id="capture-url"[^>]*inputmode="url"[^>]*autocomplete="url"/);
  assert.match(settingsHtml, /maxlength="8192" rows="4" wrap="soft"/);
  assert.match(settingsHtml, /aria-describedby="form-help" required/);
  assert.match(settingsHtml, /id="scan-pairing-button"/);
  assert.match(settingsHtml, /id="pairing-video"[^>]*playsinline/);
  assert.match(settingsHtml, /src="\/remote\/v1\/jsqr\.js"/);
  assert.match(settingsHtml, /扫描不会打开新网页或启动登录/);
  assert.doesNotMatch(settingsHtml, /代码扫描器|在 Safari 中打开|start-pairing-button/);
  assert.doesNotMatch(settingsHtml, /<input id="capture-url"/);

  const account = await request(port, '/account');
  assert.match(account.headers['content-security-policy'], /script-src 'none'/);
  assert.match(account.headers['content-security-policy'], /connect-src 'none'/);
  assert.match(account.headers['permissions-policy'], /camera=\(\)/);
  assert.match(account.body.toString('utf8'), /href="\/">← 返回资料库/);

  const css = (await request(port, '/remote/v1/app.css')).body.toString('utf8');
  assert.match(css, /\.library-layout\s*\{[\s\S]*grid-template-columns:\s*12rem minmax\(0, 1fr\)/);
  assert.match(css, /textarea\s*\{[\s\S]*min-height:\s*7rem/);
  assert.match(css, /textarea\s*\{[\s\S]*resize:\s*vertical/);
  assert.match(css, /body\[data-theme="dark"\]/);
  assert.match(css, /overflow-wrap:\s*anywhere/);
  assert.match(css, /\.library-item-button[\s\S]*border-bottom:\s*1px solid/);
  assert.doesNotMatch(css, /#f4f1e9|radial-gradient/);

  const head = await request(port, '/remote/v1/app.mjs', 'HEAD');
  assert.equal(head.status, 200);
  assert.equal(head.body.length, 0);
  assert.equal(head.headers['x-amp-review-widget'], 'off');
  const config = await request(port, '/remote/v1/config.mjs');
  assert.match(config.body.toString(), /WEB_CLIENT_URL = "https:\/\/review\.example\/"/);
  assert.match(config.body.toString(), /RELAY_URL = "wss:\/\/review\.example\/relay"/);
});

test('relay product mode serves no static client and diagnostic mode is explicit and exact', async (t) => {
  const relay = createRelay({
    port: 0,
    admissionRoot: randomBytes(32),
    browserOrigin: 'https://web.example.test',
  });
  t.after(() => relay.close());
  const { port } = await relay.listen();
  assert.equal((await request(port, '/')).status, 404);
  assert.equal((await request(port, '/remote/v1/app.mjs')).status, 404);
  assert.throws(() => createRelay({
    port: 0,
    admissionRoot: randomBytes(32),
    diagnosticPublicUrl: DIAGNOSTIC_PUBLIC_URL,
  }), /invalid_diagnostic_config/);
  assert.throws(() => createRelay({
    port: 0,
    admissionRoot: randomBytes(32),
    diagnosticPublicUrl: 'https://review.example/?query=forbidden',
    diagnosticSameOrigin: true,
  }), /invalid_diagnostic_public_url/);
});

test('URL fragments are cleared and rejected before app import so pairing cannot start a second page', async (t) => {
  const relay = diagnosticRelay();
  t.after(() => relay.close());
  const { port } = await relay.listen();
  const root = await request(port, '/');
  const html = root.body.toString('utf8');
  const fragment = `#MEMEX1:${'A'.repeat(64)}`;
  const browser = inlineBootstrap(html, { hash: fragment });
  await browser.settle();
  assert.equal(browser.location.hash, '');
  assert.equal(browser.elements.get('unsupported').hidden, false);
  assert.equal(browser.elements.has('unsupported-message'), false);
  assert.deepEqual(browser.events, ['clear']);
  assert.deepEqual(browser.starts, []);
});

test('fragment-free reload auto-resumes while malformed and query navigation fail before import', async (t) => {
  const relay = diagnosticRelay();
  t.after(() => relay.close());
  const { port } = await relay.listen();
  const root = await request(port, '/');
  const html = root.body.toString('utf8');

  const reload = inlineBootstrap(html);
  await reload.settle();
  assert.deepEqual(reload.events, ['import:/remote/v1/app.mjs', 'startLibrary']);
  assert.deepEqual(reload.starts, [{
    pageHref: 'https://review.example/',
  }]);

  for (const input of [
    { hash: '#MEMEX1:not+base64url' },
    { hash: `#MEMEX1:${'A'.repeat(64)}`, search: '?forbidden=1' },
  ]) {
    const rejected = inlineBootstrap(html, input);
    await rejected.settle();
    assert.equal(rejected.location.hash, '');
    assert.equal(rejected.location.search, '');
    assert.deepEqual(rejected.events, ['clear']);
    assert.deepEqual(rejected.starts, []);
    assert.equal(rejected.elements.get('unsupported').hidden, false);
  }

  for (const path of [
    '/?relay=wss%3A%2F%2Fevil.example%2Frelay',
    '/remote/v1/app.mjs?cap=forbidden',
    '/remote/v1/../protocol.mjs',
    '/remote/v1/not-found.mjs',
  ]) {
    const response = await request(port, path);
    assert.equal(response.status, 404, path);
    assert.equal(response.body.toString(), 'not_found', path);
  }
  const health = await request(port, '/healthz');
  assert.equal(health.status, 200);
  assert.equal(health.headers['x-amp-review-widget'], undefined);
});

test('browser and production transport sources expose no payload logging sink', () => {
  const root = path.join(__dirname, '..');
  const browserSource = ['app.mjs', 'protocol.mjs', 'storage.mjs', 'scanner.mjs']
    .map((file) => fs.readFileSync(path.join(root, 'web', file), 'utf8'))
    .join('\n');
  assert.doesNotMatch(browserSource, /\b(?:console\.|localStorage|sendBeacon\b)/);

  for (const relative of [
    'relay/server.js', 'mac/bridge.js', 'mac/secure-session.js', 'mac/rpc-router.js',
  ]) {
    const source = fs.readFileSync(path.join(root, relative), 'utf8');
    const outputCalls = source.match(/process\.(?:stdout|stderr)\.write\([^\n]+/g) || [];
    for (const call of outputCalls) {
      assert.doesNotMatch(
        call,
        /JSON\.stringify|payload|ciphertext|session_id|bootstrap_secret|\burl\b|markdown|body/,
        `${relative}: ${call}`,
      );
    }
    assert.doesNotMatch(source, /\bconsole\.(?:log|info|debug|warn|error)\s*\(/, relative);
  }
});

test('mobile manifest serves the default brand module unless the owner names the product', () => {
  const root = path.join(__dirname, '../../mobile/web');
  const { mobileRoutes } = require('../service/mobile-gateway');
  const served = (brand) => mobileRoutes(root, brand).get('/mobile/brand.mjs').body.toString('utf8');
  assert.equal(served(), fs.readFileSync(path.join(root, 'brand.mjs'), 'utf8'));
  assert.match(served(), /BRAND_NAME = 'Lots MemeX'/);
  const named = served({ name: 'Private Name', assistant: '' });
  assert.match(named, /^export const BRAND_NAME = "Private Name";$/m);
  assert.match(named, /^export const ASSISTANT_NAME = "MemeX";$/m);
  for (const name of ['x";alert(1)//', 'a\nb', '<b>', 'x'.repeat(41)]) {
    assert.throws(() => mobileRoutes(root, { name }), /invalid_brand_name/);
  }
});

test('loopback mobile manifest pins its actual origin and serves no arbitrary files', async (t) => {
  const root = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'memex-mobile-assets-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'assets'));
  for (const file of ['index.html', 'app.mjs', 'app.css', 'gateway-source.mjs', 'demo-source.mjs',
    'fixtures.mjs', 'i18n.mjs', 'brand.mjs', 'assets/icon-192.png', 'assets/apple-touch-icon.png',
    'assets/memex-avatar.png', 'changelog.mjs', 'assets/memex-portrait.png', 'config.json', 'zhconv.mjs',
    'assets/opencc/STCharacters.txt', 'assets/opencc/STPhrases.txt', 'assets/opencc/TSCharacters.txt',
    'assets/opencc/TSPhrases.txt', 'assets/opencc/LICENSE-OpenCC.txt',
    'assets/fonts/ChillZhuoKai.woff2', 'assets/fonts/OFL-ChillZhuoKai.txt']) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), file === 'index.html' ? '<h1>Synthetic mobile</h1>' : file);
  }
  const application = { path: '/mobile/index.html',
    routes: require('../service/mobile-gateway').mobileRoutes(root) };
  // A public HTTPS origin may mount the manifest, but the relay still binds loopback only.
  assert.doesNotThrow(() => diagnosticRelay({ application }));
  assert.throws(() => diagnosticRelay({ application, host: '0.0.0.0' }), /invalid_diagnostic_config/);
  assert.throws(() => diagnosticRelay({ application, host: '0.0.0.0',
    diagnosticPublicUrl: 'http://127.0.0.1:0/' }), /invalid_diagnostic_config/);
  const relay = diagnosticRelay({ application, diagnosticPublicUrl: 'http://127.0.0.1:0/' });
  const { port } = await relay.listen();
  t.after(() => relay.close());
  const page = await request(port, '/mobile/index.html');
  assert.equal(page.body.toString(), '<h1>Synthetic mobile</h1>');
  assert.match(page.headers['content-security-policy'], /script-src 'self';/);
  assert.match(page.headers['content-security-policy'], /connect-src 'self' ws:\/\/127\.0\.0\.1:/);
  assert.match(page.headers['content-security-policy'], /font-src 'self';/);
  assert.doesNotMatch(page.headers['content-security-policy'], /unsafe-inline|unsafe-eval/);
  const config = (await request(port, '/remote/v1/config.mjs')).body.toString();
  assert(config.includes(`http://127.0.0.1:${port}/mobile/index.html`));
  assert(config.includes(`ws://127.0.0.1:${port}/relay`));
  assert.equal((await request(port, '/remote/v1/protocol.mjs')).status, 200);
  for (const [file, type] of [['changelog.mjs', 'text/javascript; charset=utf-8'],
    ['zhconv.mjs', 'text/javascript; charset=utf-8'],
    ['assets/fonts/ChillZhuoKai.woff2', 'font/woff2'],
    ...['assets/opencc/STCharacters.txt', 'assets/opencc/STPhrases.txt',
      'assets/opencc/TSCharacters.txt', 'assets/opencc/TSPhrases.txt',
      'assets/opencc/LICENSE-OpenCC.txt', 'assets/fonts/OFL-ChillZhuoKai.txt']
      .map(file => [file, 'text/plain; charset=utf-8']),
    ['assets/memex-portrait.png', 'image/png']]) {
    const response = await request(port, `/mobile/${file}`);
    assert.equal(response.status, 200);
    assert.equal(response.headers['content-type'], type);
    assert.equal(response.body.toString(), file);
  }
  for (const file of ['/mobile/../../config.json', '/mobile/config.json', '/agent.db',
    '/mobile/index.html?bootstrap=forbidden']) assert.equal((await request(port, file)).status, 404);
});

test('handoff states the distinct native and Safari threat and durability boundaries', () => {
  const readme = fs.readFileSync(path.join(__dirname, '../README.md'), 'utf8');
  assert.match(readme, /signed native client provides end-to-end encryption against an active relay operator/);
  assert.match(readme, /Safari diagnostic encrypts frames only after trusted HTTPS code delivery/);
  assert.match(readme, /trusts its TLS ingress\/static-code provider against active substitution/);
  assert.match(readme, /new Quick Tunnel hostname is a new browser origin/);
  assert.match(readme, /Stable production remote use requires the signed native client/);
  assert.match(readme, /one exclusive Web Lock serializes the active tab/);
  assert.doesNotMatch(readme, /Safari[^\n]*(?:blind|untrusted-relay) E2EE/i);
});
