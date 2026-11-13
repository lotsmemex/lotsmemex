'use strict';

const { createHmac, timingSafeEqual } = require('node:crypto');
const { base64url, decodeBase64url, lengthPrefixed } = require('./protocol');

const ADMISSION_DOMAIN = 'memex-relay-admission-v1';
const FIXED_SUBPROTOCOL = 'memex.remote.v1';
const ROLE_PREFIX = 'memex.role.';
const CLIENT_PREFIX = 'memex.client.';
const CAPABILITY_PREFIX = 'memex.cap.';

function admissionRoot(value) {
  if (Buffer.isBuffer(value)) {
    if (value.length !== 32) throw new Error('invalid_admission_root');
    return Buffer.from(value);
  }
  return decodeBase64url(value, 32, 'invalid_admission_root');
}

function validClientMode(role, clientMode) {
  return (role === 'mac' && clientMode === 'bridge')
    || (role === 'iphone' && ['native', 'browser'].includes(clientMode));
}

function deriveCapability(root, sessionId, role, clientMode) {
  if (!['mac', 'iphone'].includes(role)) throw new Error('invalid_admission_role');
  if (!validClientMode(role, clientMode)) throw new Error('invalid_admission_client');
  const session = decodeBase64url(sessionId, 32, 'invalid_session');
  return base64url(createHmac('sha256', admissionRoot(root)).update(lengthPrefixed([
    ADMISSION_DOMAIN,
    session,
    role,
    clientMode,
  ])).digest());
}

function clientProtocols(role, clientMode, capability) {
  decodeBase64url(capability, 32, 'invalid_admission_capability');
  if (!['mac', 'iphone'].includes(role)) throw new Error('invalid_admission_role');
  if (!validClientMode(role, clientMode)) throw new Error('invalid_admission_client');
  return [
    FIXED_SUBPROTOCOL,
    `${ROLE_PREFIX}${role}`,
    `${CLIENT_PREFIX}${clientMode}`,
    `${CAPABILITY_PREFIX}${capability}`,
  ];
}

function authorizeProtocols(header, root, sessionId) {
  if (typeof header !== 'string' || header.length > 256) return null;
  const offered = header.split(',').map((value) => value.trim());
  if (offered.length !== 4 || new Set(offered).size !== 4 || !offered.includes(FIXED_SUBPROTOCOL)) {
    return null;
  }
  const roleToken = offered.find((value) => value.startsWith(ROLE_PREFIX));
  const clientToken = offered.find((value) => value.startsWith(CLIENT_PREFIX));
  const capabilityToken = offered.find((value) => value.startsWith(CAPABILITY_PREFIX));
  const role = roleToken?.slice(ROLE_PREFIX.length);
  const clientMode = clientToken?.slice(CLIENT_PREFIX.length);
  const capability = capabilityToken?.slice(CAPABILITY_PREFIX.length);
  if (!validClientMode(role, clientMode) || !capability) return null;
  let actual;
  let expected;
  try {
    actual = decodeBase64url(capability, 32, 'invalid_admission_capability');
    expected = Buffer.from(deriveCapability(root, sessionId, role, clientMode), 'base64url');
  } catch {
    return null;
  }
  return timingSafeEqual(actual, expected) ? { role, clientMode } : null;
}

module.exports = {
  ADMISSION_DOMAIN,
  CLIENT_PREFIX,
  FIXED_SUBPROTOCOL,
  admissionRoot,
  authorizeProtocols,
  clientProtocols,
  deriveCapability,
};
