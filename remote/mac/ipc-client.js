'use strict';

const net = require('node:net');

const MAX_IPC_REQUEST_FRAME = 64 * 1024;
const MAX_IPC_RESPONSE_FRAME = 320 * 1024;

function sendOwnerCommand(socketPath, command, { timeoutMs = 5_000 } = {}) {
  const request = Buffer.from(JSON.stringify(command), 'utf8');
  if (request.length === 0 || request.length > MAX_IPC_REQUEST_FRAME) {
    return Promise.reject(new Error('invalid_request'));
  }
  const frame = Buffer.alloc(4 + request.length);
  frame.writeUInt32BE(request.length);
  request.copy(frame, 4);
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ path: socketPath });
    let buffer = Buffer.alloc(0);
    let expected = null;
    let settled = false;
    const finish = (error, response) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (error) reject(error); else resolve(response);
    };
    socket.setTimeout(timeoutMs, () => finish(new Error('daemon_unavailable')));
    socket.on('error', () => finish(new Error('daemon_unavailable')));
    socket.on('connect', () => socket.write(frame));
    socket.on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (expected === null && buffer.length >= 4) {
        expected = buffer.readUInt32BE();
        buffer = buffer.subarray(4);
        if (expected === 0 || expected > MAX_IPC_RESPONSE_FRAME) {
          finish(new Error('invalid_daemon_response'));
          return;
        }
      }
      if (expected !== null && buffer.length >= expected) {
        if (buffer.length !== expected) {
          finish(new Error('invalid_daemon_response'));
          return;
        }
        try {
          finish(null, JSON.parse(buffer.toString('utf8')));
        } catch {
          finish(new Error('invalid_daemon_response'));
        }
      }
    });
    socket.on('end', () => {
      if (!settled) finish(new Error('invalid_daemon_response'));
    });
  });
}

module.exports = { MAX_IPC_REQUEST_FRAME, MAX_IPC_RESPONSE_FRAME, sendOwnerCommand };
