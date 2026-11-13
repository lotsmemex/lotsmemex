'use strict';
const { spawn } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');

async function startChromeFixture() {
  const cwd = path.resolve(__dirname, '../..');
  const child = spawn(path.join(cwd, 'target/debug/examples/chrome_fixture'), [], {
    cwd, env: { ...process.env, TMPDIR: '/private/tmp' }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  const lines = readline.createInterface({ input: child.stdout });
  const pending = new Map();
  let receipt;
  let sequence = 0;
  let closed = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    if (child.pid && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, 'exit');
      child.kill('SIGTERM');
      await exited;
    }
    lines.close();
    for (const { reject, timer } of pending.values()) { clearTimeout(timer); reject(new Error('fixture_closed')); }
    pending.clear();
    if (receipt?.root && path.dirname(receipt.root) === '/private/tmp'
      && path.basename(receipt.root).startsWith('memex-chrome-')) {
      const marker = JSON.parse(fs.readFileSync(path.join(receipt.root, 'synthetic-receipt.json'), 'utf8'));
      if (marker.synthetic_only && marker.pid === child.pid && marker.root === receipt.root) {
        fs.rmSync(receipt.root, { recursive: true });
      }
    }
  };
  try {
    receipt = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('fixture_start_timeout')), 15_000);
      child.once('error', error => { clearTimeout(timer); reject(error); });
      child.once('exit', () => { clearTimeout(timer); reject(new Error('fixture_start_failed')); });
      child.stderr.resume();
      lines.once('line', line => {
        clearTimeout(timer);
        try { resolve(JSON.parse(line)); } catch (error) { reject(error); }
      });
    });
    if (!receipt.synthetic_only || receipt.pid !== child.pid) throw new Error('invalid_fixture');
    lines.on('line', line => {
      const response = JSON.parse(line);
      const waiting = pending.get(response.id);
      if (waiting) { clearTimeout(waiting.timer); pending.delete(response.id); waiting.resolve(response.result); }
    });
    function command(command) {
      return new Promise((resolve, reject) => {
        const id = ++sequence;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error('fixture_command_timeout')); }, 15_000);
        pending.set(id, { resolve, reject, timer });
        child.stdin.write(`${JSON.stringify({ id, command })}\n`);
      });
    }
    return { ...receipt, baseUrl: new URL('/', receipt.url).href, close,
      pause: () => command('pause'), resume: () => command('resume'), inspect: () => command('inspect') };
  } catch (error) { await close(); throw error; }
}
module.exports = { startChromeFixture };
