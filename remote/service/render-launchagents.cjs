'use strict';

const { dataDir } = require('../data-dir');
const fs = require('node:fs');
const path = require('node:path');

const NAMES = Object.freeze([
  'com.lotsmemex.remote-relay.plist',
  'com.lotsmemex.remote-bridge.plist',
  'com.lotsmemex.remote-tunnel.plist',
]);

function xml(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&apos;');
}

function ownerDirectory(directory) {
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const stat = fs.lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid()
    || (stat.mode & 0o077) !== 0) throw new Error('unsafe_destination');
}

function render(repositoryRoot, destination) {
  const rootStat = fs.lstatSync(repositoryRoot);
  const root = fs.realpathSync(repositoryRoot);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink() || root !== repositoryRoot
    || rootStat.uid !== process.getuid()) throw new Error('unsafe_repository');
  const source = path.join(root, 'remote/service');
  if (!fs.statSync(path.join(source, 'run-service.sh')).isFile()) throw new Error('invalid_repository');
  ownerDirectory(destination);
  for (const name of NAMES) {
    const template = fs.readFileSync(path.join(source, `${name}.template`), 'utf8');
    if (template.split('__REPOSITORY_ROOT__').length !== 2) throw new Error('invalid_template');
    const body = template.replace('__REPOSITORY_ROOT__', xml(root));
    const target = path.join(destination, name);
    const temporary = `${target}.tmp-${process.pid}`;
    const handle = fs.openSync(temporary, fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_WRONLY, 0o600);
    try {
      fs.writeFileSync(handle, body, 'utf8');
      fs.fsyncSync(handle);
    } finally {
      fs.closeSync(handle);
    }
    fs.renameSync(temporary, target);
    fs.chmodSync(target, 0o600);
  }
  return NAMES.map((name) => path.join(destination, name));
}

if (require.main === module) {
  try {
    const root = fs.realpathSync(process.argv[2] || process.cwd());
    const destination = path.join(dataDir(), 'remote-managed/launchagents');
    render(root, destination);
    process.stdout.write('launchagents_rendered\n');
  } catch {
    process.stderr.write('launchagent_render_failed\n');
    process.exitCode = 1;
  }
}

module.exports = { NAMES, render };
