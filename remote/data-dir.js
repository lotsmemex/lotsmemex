'use strict';

const os = require('node:os');
const path = require('node:path');

// Folder under ~/Library/Application Support that holds the daemon socket and remote state.
// Defaults to LotsMemeX; an install that keeps an older data directory sets MEMEX_DATA_DIR_NAME.
function dataDir(env = process.env) {
  const name = env.MEMEX_DATA_DIR_NAME || 'LotsMemeX';
  if (!/^[A-Za-z0-9][A-Za-z0-9 ._-]{0,63}$/.test(name) || name.includes('..')) {
    throw new Error('invalid_data_dir_name');
  }
  return path.join(os.homedir(), 'Library/Application Support', name);
}

module.exports = { dataDir };
