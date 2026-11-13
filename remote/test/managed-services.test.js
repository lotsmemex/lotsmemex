'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const { NAMES, render } = require('../service/render-launchagents.cjs');

const root = path.resolve(__dirname, '../..');
const serviceRoot = path.join(root, 'remote/service');

function writeExecutable(file, body) {
  fs.writeFileSync(file, body, { mode: 0o700 });
  fs.chmodSync(file, 0o700);
}

function shellValue(value) {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

function runControl(home, bin, action, extraEnv = {}) {
  return spawnSync('/bin/sh', [path.join(serviceRoot, 'service-control.sh'), action, 'relay'], {
    encoding: 'utf8',
    env: {
      ...process.env,
      HOME: home,
      PATH: `${bin}:/usr/bin:/bin`,
      ...extraEnv,
    },
  });
}

function plist(label) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>${label}</string></dict></plist>\n`;
}

test('managed service shells are syntactically valid, exact-label only, and never broadly kill', () => {
  for (const name of ['run-service.sh', 'preflight.sh', 'service-control.sh']) {
    const file = path.join(serviceRoot, name);
    assert.equal(spawnSync('/bin/sh', ['-n', file]).status, 0, name);
    const source = fs.readFileSync(file, 'utf8');
    assert.doesNotMatch(source, /\b(?:pkill|killall|sudo)\b|\bkill\s+-/);
    assert.doesNotMatch(source, /MEMEX_RELAY_ADMISSION_ROOT.*(?:printf|echo)/);
  }
  const control = fs.readFileSync(path.join(serviceRoot, 'service-control.sh'), 'utf8');
  for (const label of [
    'com.lotsmemex.remote-relay', 'com.lotsmemex.remote-bridge',
    'com.lotsmemex.remote-tunnel',
  ]) assert.match(control, new RegExp(label.replaceAll('.', '\\.')));
  assert.doesNotMatch(control, /[a-z]\.(?:agent|surface)\b/);
  assert.match(control, /launchctl print "\$domain\/\$label"/);
  assert.match(control, /launchctl bootout "\$domain" "\$plist"/);
  assert.match(control, /\$\(id -u\) 600 1 Regular File/);
  assert.match(control, /PlistBuddy -c 'Print :Label'/);
});

test('plist render is owner-private and argv contains only exact wrapper plus role', () => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-remote-services-'));
  const destination = path.join(temp, 'private');
  try {
    const rendered = render(root, destination);
    assert.deepEqual(rendered.map((file) => path.basename(file)), NAMES);
    assert.equal(fs.statSync(destination).mode & 0o777, 0o700);
    for (const file of rendered) {
      assert.equal(fs.statSync(file).mode & 0o777, 0o600);
      assert.equal(spawnSync('/usr/bin/plutil', ['-lint', file]).status, 0, file);
      const body = fs.readFileSync(file, 'utf8');
      assert.doesNotMatch(body, /__REPOSITORY_ROOT__|ADMISSION|credentials-file|relay\.example\.test/);
      assert.match(body, /<key>ProgramArguments<\/key><array>\s*<string>[^<]+\/remote\/service\/run-service\.sh<\/string><string>(?:relay|bridge|tunnel)<\/string>\s*<\/array>/);
      assert.doesNotMatch(body, /[a-z]\.(?:agent|surface)\b/);
      assert.match(body, /<key>StandardOutPath<\/key><string>\/dev\/null<\/string>/);
      assert.match(body, /<key>StandardErrorPath<\/key><string>\/dev\/null<\/string>/);
    }
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
});

test('templates preserve loopback, owner-file, stable-host, and migration contracts', () => {
  const runner = fs.readFileSync(path.join(serviceRoot, 'run-service.sh'), 'utf8');
  const preflight = fs.readFileSync(path.join(serviceRoot, 'preflight.sh'), 'utf8');
  const tunnel = fs.readFileSync(path.join(serviceRoot, 'cloudflared-config.yml.template'), 'utf8');
  const runbook = fs.readFileSync(path.join(serviceRoot, 'README.md'), 'utf8');
  assert.match(runner, /export HOST=127\.0\.0\.1/);
  assert.match(runner, /unset MEMEX_RELAY_TRUST_PROXY/);
  assert.match(runner, /MEMEX_BROWSER_ORIGIN="\$MEMEX_REMOTE_BROWSER_ORIGIN"/);
  assert.match(runner, /MEMEX_RELAY_URL="\$MEMEX_REMOTE_RELAY_URL"/);
  assert.match(runner, /MEMEX_WEB_CLIENT_URL="\$MEMEX_REMOTE_WEB_CLIENT_URL"/);
  assert.match(runner, /exec "\$MEMEX_CLOUDFLARED_EXECUTABLE"/);
  assert.match(runner, /unset MEMEX_RELAY_ADMISSION_ROOT MEMEX_AGENT_SOCKET/);
  assert.match(preflight, /600 1 Regular File/);
  assert.match(preflight, /700 Directory/);
  assert.match(preflight, /600 Socket/);
  assert.match(preflight, /rev-parse HEAD/);
  assert.match(preflight, /status --porcelain/);
  assert.match(preflight, /grep -q 'MEMEX_RELAY_TRUST_PROXY'/);
  assert.match(tunnel, /hostname: __RELAY_HOSTNAME__/);
  assert.match(preflight, /hostname: \$MEMEX_REMOTE_RELAY_HOSTNAME/);
  assert.match(tunnel, /service: http:\/\/127\.0\.0\.1:__HIGH_LOOPBACK_PORT__/);
  assert.match(tunnel, /service: http_status:404/);
  // The phone-access runbook a user's agent follows: exact ingress, loopback-only gateway,
  // every required gateway variable, and a fresh pair on a new origin.
  assert.match(runbook, /- hostname: phone\.example\.com\n    service: http:\/\/127\.0\.0\.1:48731\n  - service: http_status:404/);
  assert.match(runbook, /只监听 `127\.0\.0\.1`/);
  assert.match(runbook, /二维码不是网址/);
  assert.match(runbook, /需要重新配对/);
  for (const name of ['MEMEX_AGENT_SOCKET', 'MEMEX_MOBILE_PUBLIC_URL', 'MEMEX_MOBILE_ROOT',
    'MEMEX_MOBILE_STATE_DIR', 'MEMEX_MOBILE_PORT']) {
    assert.ok(runbook.includes(`| \`${name}\` | 是 |`), name);
  }
});

test('relay runner strips inherited proxy trust and preflight rejects it in owner config', async () => {
  const temp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'memex-remote-runner-')));
  const home = path.join(temp, 'home');
  const configDirectory = path.join(home, 'Library/Application Support/LotsMemeX/remote-managed');
  const repository = path.join(temp, 'repository');
  const bin = path.join(temp, 'bin');
  const socketPath = path.join(temp, 'agent.sock');
  const configPath = path.join(configDirectory, 'remote-service.env');
  const tunnelConfig = path.join(configDirectory, 'cloudflared.yml');
  const credentials = path.join(configDirectory, 'credentials.json');
  const fakeNode = path.join(bin, 'node');
  const fakeCloudflared = path.join(bin, 'cloudflared');
  let server;
  try {
    fs.mkdirSync(path.join(repository, 'remote/relay'), { recursive: true });
    fs.mkdirSync(path.join(repository, 'remote/mac'), { recursive: true });
    fs.mkdirSync(configDirectory, { recursive: true, mode: 0o700 });
    fs.chmodSync(configDirectory, 0o700);
    fs.mkdirSync(bin, { mode: 0o700 });
    fs.writeFileSync(path.join(repository, 'remote/relay/server.js'), 'void 0;\n');
    fs.writeFileSync(path.join(repository, 'remote/mac/bridge.js'), 'void 0;\n');
    assert.equal(spawnSync('git', ['init', '-q'], { cwd: repository }).status, 0);
    assert.equal(spawnSync('git', ['add', '.'], { cwd: repository }).status, 0);
    assert.equal(spawnSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
      'commit', '-qm', 'fixture'], { cwd: repository }).status, 0);
    const commit = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: repository, encoding: 'utf8' }).stdout.trim();

    writeExecutable(fakeNode, `#!/bin/sh
if env | grep -q '^MEMEX_RELAY_TRUST_PROXY='; then
  printf '%s\\n' relay_trust_proxy_present
  exit 1
fi
printf '%s\\n' relay_trust_proxy_absent
`);
    writeExecutable(fakeCloudflared, '#!/bin/sh\nexit 0\n');
    fs.writeFileSync(credentials, '{}\n', { mode: 0o600 });
    fs.chmodSync(credentials, 0o600);
    fs.writeFileSync(tunnelConfig, `tunnel: synthetic
credentials-file: ${credentials}
ingress:
  - hostname: relay.example.test
    service: http://127.0.0.1:49373
  - service: http_status:404
`, { mode: 0o600 });
    fs.chmodSync(tunnelConfig, 0o600);
    fs.writeFileSync(configPath, [
      `MEMEX_REMOTE_REPOSITORY_ROOT=${shellValue(repository)}`,
      `MEMEX_REMOTE_EXPECTED_COMMIT=${shellValue(commit)}`,
      `MEMEX_REMOTE_NODE=${shellValue(fakeNode)}`,
      `MEMEX_RELAY_ADMISSION_ROOT=${shellValue('A'.repeat(43))}`,
      `MEMEX_REMOTE_RELAY_PORT=${shellValue('49373')}`,
      `MEMEX_AGENT_SOCKET=${shellValue(socketPath)}`,
      `MEMEX_CLOUDFLARED_EXECUTABLE=${shellValue(fakeCloudflared)}`,
      `MEMEX_CLOUDFLARED_CONFIG=${shellValue(tunnelConfig)}`,
      `MEMEX_CLOUDFLARED_CREDENTIALS=${shellValue(credentials)}`,
      `MEMEX_REMOTE_BROWSER_ORIGIN=${shellValue('https://web.example.test')}`,
      `MEMEX_REMOTE_WEB_CLIENT_URL=${shellValue('https://web.example.test/remote/v1/index.html')}`,
      `MEMEX_REMOTE_RELAY_URL=${shellValue('wss://relay.example.test/relay')}`,
      `MEMEX_REMOTE_RELAY_HOSTNAME=${shellValue('relay.example.test')}`,
      '',
    ].join('\n'), { mode: 0o600 });
    fs.chmodSync(configPath, 0o600);

    server = net.createServer();
    await new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(socketPath, resolve);
    });
    fs.chmodSync(socketPath, 0o600);

    const invoked = spawnSync('/bin/sh', [path.join(serviceRoot, 'run-service.sh'), 'relay'], {
      encoding: 'utf8',
      env: { ...process.env, HOME: home, MEMEX_RELAY_TRUST_PROXY: '1' },
    });
    assert.equal(invoked.status, 0, invoked.stderr);
    assert.equal(invoked.stdout, 'relay_trust_proxy_absent\n');

    fs.appendFileSync(configPath, "MEMEX_RELAY_TRUST_PROXY='1'\n");
    const rejected = spawnSync('/bin/sh', [path.join(serviceRoot, 'preflight.sh')], {
      encoding: 'utf8', env: { ...process.env, HOME: home },
    });
    assert.equal(rejected.status, 1);
    assert.equal(rejected.stdout, '');
    assert.equal(rejected.stderr, 'managed_services_preflight_failed\n');
  } finally {
    if (server) await new Promise((resolve) => server.close(resolve));
    fs.rmSync(temp, { recursive: true, force: true });
  }
});

test('service control accepts only owner-private exact-label plists for start and stop', () => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'memex-remote-control-'));
  const home = path.join(temp, 'home');
  const launchAgents = path.join(home, 'Library/LaunchAgents');
  const bin = path.join(temp, 'bin');
  const log = path.join(temp, 'launchctl.log');
  const selected = path.join(launchAgents, 'com.lotsmemex.remote-relay.plist');
  try {
    fs.mkdirSync(launchAgents, { recursive: true });
    fs.mkdirSync(bin);
    writeExecutable(path.join(bin, 'launchctl'), '#!/bin/sh\nprintf "%s\\n" "$*" >> "$LAUNCHCTL_LOG"\n');
    fs.writeFileSync(selected, plist('com.lotsmemex.remote-relay'), { mode: 0o600 });
    fs.chmodSync(selected, 0o600);

    for (const action of ['start', 'stop']) {
      const result = runControl(home, bin, action, { LAUNCHCTL_LOG: log });
      assert.equal(result.status, 0, result.stderr);
    }
    const calls = fs.readFileSync(log, 'utf8').trim().split('\n');
    assert.deepEqual(calls, [
      `bootstrap gui/${process.getuid()} ${selected}`,
      `bootout gui/${process.getuid()} ${selected}`,
    ]);

    const assertRejected = (extraEnv = {}) => {
      const before = fs.readFileSync(log, 'utf8');
      for (const action of ['start', 'stop']) {
        const result = runControl(home, bin, action, { LAUNCHCTL_LOG: log, ...extraEnv });
        assert.equal(result.status, 66, `${action}: ${result.stderr}`);
      }
      assert.equal(fs.readFileSync(log, 'utf8'), before);
    };

    fs.chmodSync(selected, 0o644);
    assertRejected();

    fs.rmSync(selected);
    const linked = path.join(temp, 'linked.plist');
    fs.writeFileSync(linked, plist('com.lotsmemex.remote-relay'), { mode: 0o600 });
    fs.symlinkSync(linked, selected);
    assertRejected();

    fs.rmSync(selected);
    fs.writeFileSync(selected, plist('com.lotsmemex.remote-bridge'), { mode: 0o600 });
    fs.chmodSync(selected, 0o600);
    assertRejected();

    fs.writeFileSync(selected, plist('com.lotsmemex.remote-relay'));
    fs.chmodSync(selected, 0o600);
    writeExecutable(path.join(bin, 'id'), `#!/bin/sh
if [ "\${1-}" = -u ]; then printf '%s\\n' ${process.getuid() + 1}; else exec /usr/bin/id "$@"; fi
`);
    assertRejected();
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
});
