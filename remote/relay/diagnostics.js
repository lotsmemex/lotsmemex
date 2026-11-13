'use strict';

// Never log remote close text verbatim: peers may put data or secrets in it.
const REASONS = new Set(['stopped', 'replaced', 'peer_offline', 'mac_offline',
  'peer_replaced', 'session_reset', 'limit_exceeded', 'backpressure',
  'peer_backpressure', 'message_not_delivered', 'idle_timeout', 'heartbeat_timeout',
  'peer_reset_timeout', 'peer_reset_limit',
  'invalid_frame', 'secure_transport_error', 'invalid_message', 'invalid_handshake',
  'untrusted_phone', 'reply_backpressure', 'relay_offline']);

function reasonCode(value) {
  const text = Buffer.isBuffer(value) ? value.toString('utf8') : value;
  return REASONS.has(text) ? text : 'unspecified';
}

function log(line, stream = process.stdout) {
  // Callers pass fixed event names and bounded numeric fields only.
  stream.write(`${new Date().toISOString()} ${line}\n`);
}

function closed(role, code, reason) {
  log(`memex_transport_closed role=${role === 'mac' ? 'mac' : 'iphone'} code=${Number.isInteger(code) ? code : 1006} reason=${reasonCode(reason)}`);
}

module.exports = { log, closed, reasonCode };
