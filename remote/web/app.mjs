import {
  FIXED_SUBPROTOCOL,
  WebSecureSession,
  assertExactKeys,
  base64url,
  browserSubprotocols,
  parsePairingCode,
  relaySocketUrl,
  validatePersistedPairing,
} from './protocol.mjs';
import {
  acquireBrowserLease,
  compareAndSetOutbox,
  deleteOutboxByIdentity,
  deleteState,
  getState,
  loadOrCreateIdentity,
  loadOrCreatePhoneId,
  openDatabase,
  putState,
  runCapabilityProbe,
} from './storage.mjs';
import { ALLOW_LOCAL, RELAY_URL, WEB_CLIENT_URL } from './config.mjs';
import { scanPairingCode } from './scanner.mjs';

const RPC_TIMEOUT_MS = 10_000;
const POLL_INTERVAL_MS = 5_000;
const MAX_AUTO_POLLS = 120;
const MAX_LIBRARY_PAGE_ITEMS = 50;
const JOB_PATTERN = /^job_[a-f0-9]{32}$/;
const ITEM_PATTERN = /^item_[A-Za-z0-9_-]{1,128}$/;
const ID_PATTERN = /^[A-Za-z0-9_.-]{1,128}$/;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;
const RUNNING_STATES = new Set(['queued', 'leased', 'artifact_ready', 'retry_wait']);
const TERMINAL_STATES = new Set(['succeeded', 'failed', 'cancelled']);

const ERROR_MESSAGES = Object.freeze({
  invalid_navigation: '请直接打开已登录的配对页，不要添加网址参数或片段。',
  invalid_pairing: '配对码无效或已过期。请在 Mac 上生成新的配对码。',
  scanner_unavailable: '当前 Safari 无法启动安全扫描器。',
  camera_unavailable: '无法使用相机。请允许此页面访问相机后重试。',
  scan_timeout: '扫描已超时。请重新开始扫描。',
  invalid_url: '请输入一个不含用户名或密码的公开 HTTPS 网址。',
  pending_exists: '请先停止检查当前采集，再提交新的网址。',
  mac_offline: 'Mac 当前离线。本次请求尚未确认送达。',
  daemon_unavailable: 'Mac 采集服务当前不可用。',
  request_identity_conflict: '保存的请求与 Mac 先前接收的任务不匹配。',
  remote_resource_not_found: '无法读取该远程结果。',
  library_resource_not_found: '该资料已不存在。请刷新资料库。',
  library_scan_limit_exceeded: '资料库超过当前安全读取上限，已停止加载。',
  invalid_cursor: '资料库已发生变化。请重新刷新。',
  result_too_large: '已存储结果过大，无法在此客户端读取。',
  another_tab_active: '另一个标签页正在使用 Lots MemeX 远程采集。请关闭它后重试。',
  outbox_conflict: '另一个标签页已更改保存的采集状态。请刷新页面后重试。',
  storage_unavailable: '私密浏览器存储当前不可用。',
  unsupported_secure_context: '能力检查 S1 失败：需要受信任的 HTTPS 浏览环境。',
  unsupported_webcrypto: '能力检查 S2 失败：缺少所需的 WebCrypto 接口。',
  unsupported_web_locks: '能力检查 S3 失败：缺少同源范围的 Web Locks。',
  unsupported_ed25519: '能力检查 S4 失败：无法使用 Ed25519 签名。',
  unsupported_x25519: '能力检查 S5 失败：无法使用 X25519 密钥协商。',
  unsupported_hkdf_aes_gcm: '能力检查 S6 失败：无法使用 HKDF 或带认证的 AES-GCM。',
  unsupported_indexeddb_write: '能力检查 S7 失败：私密浏览器存储无法持久写入密钥。',
  unsupported_indexeddb_reopen: '能力检查 S8 失败：私密浏览器存储无法重新打开已保存的密钥。',
  unsupported_indexeddb_cryptokey: '能力检查 S9 失败：无法再次使用已保存的不可提取密钥。',
  unsupported_browser: '需要 Safari 18.4 或更高版本，并关闭此网站的无痕浏览。',
});

function element(id, documentValue = document) {
  const value = documentValue.getElementById(id);
  if (!value) throw new Error('invalid_document');
  return value;
}

function optionalElement(id, documentValue = document) {
  return documentValue.getElementById(id);
}

function validateExactProductPage(pageHref, webClientUrl, suffix = '') {
  let page;
  let home;
  try {
    page = new URL(pageHref);
    home = new URL(webClientUrl);
  } catch {
    throw new Error('invalid_navigation');
  }
  const base = home.pathname === '/' ? '' : home.pathname;
  const expectedPath = suffix ? `${base}/${suffix}` : home.pathname;
  if (
    page.origin !== home.origin
    || page.pathname !== expectedPath
    || page.search || page.hash
    || home.search || home.hash
  ) throw new Error('invalid_navigation');
}

export function validateLibraryPage(pageHref, webClientUrl) {
  validateExactProductPage(pageHref, webClientUrl);
}

export function validateSettingsPage(pageHref, webClientUrl) {
  validateExactProductPage(pageHref, webClientUrl, 'settings');
}

export function validateCaptureUrl(value) {
  if (typeof value !== 'string' || new TextEncoder().encode(value).length > 8 * 1024) {
    throw new Error('invalid_url');
  }
  let url;
  try { url = new URL(value); } catch { throw new Error('invalid_url'); }
  if (
    url.protocol !== 'https:'
    || url.username
    || url.password
    || !url.hostname
    || url.hash
  ) throw new Error('invalid_url');
  return url.toString();
}

function boundedText(value, limit) {
  return typeof value === 'string'
    && new TextEncoder().encode(value).length <= limit
    && !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(value);
}

export function validateLibraryListResult(value) {
  assertExactKeys(value, ['items', 'next_cursor'], 'invalid_response');
  if (!Array.isArray(value.items) || value.items.length > MAX_LIBRARY_PAGE_ITEMS) {
    throw new Error('invalid_response');
  }
  for (const item of value.items) {
    assertExactKeys(
      item,
      ['item_id', 'revision_id', 'title', 'item_type', 'captured_at', 'site_name'],
      'invalid_response',
    );
    if (
      !ITEM_PATTERN.test(item.item_id)
      || !ID_PATTERN.test(item.revision_id)
      || !boundedText(item.title, 4 * 1024)
      || !boundedText(item.item_type, 128)
      || (item.site_name !== null && !boundedText(item.site_name, 256))
      || (item.captured_at !== null && (
        !boundedText(item.captured_at, 64)
        || !RFC3339_PATTERN.test(item.captured_at)
        || !Number.isFinite(Date.parse(item.captured_at))
      ))
    ) throw new Error('invalid_response');
  }
  if (value.next_cursor !== null && (
    !ITEM_PATTERN.test(value.next_cursor)
    || value.items.length === 0
    || value.items.at(-1).item_id !== value.next_cursor
  )) throw new Error('invalid_response');
  return value;
}

export function validateLibraryItemResult(value, expectedItemId) {
  assertExactKeys(
    value,
    [
      'item_id', 'revision_id', 'summary_state', 'summary',
      'translation_state', 'title', 'markdown',
    ],
    'invalid_response',
  );
  const states = new Set([
    'not_requested', 'queued', 'running', 'succeeded', 'failed', 'unavailable',
  ]);
  if (
    value.item_id !== expectedItemId
    || !ITEM_PATTERN.test(value.item_id)
    || !ID_PATTERN.test(value.revision_id)
    || !states.has(value.summary_state)
    || !states.has(value.translation_state)
  ) throw new Error('invalid_response');
  if (value.summary_state === 'succeeded') {
    if (!boundedText(value.summary, 8 * 1024)) throw new Error('invalid_response');
  } else if (value.summary !== null) {
    throw new Error('invalid_response');
  }
  if (value.translation_state === 'succeeded') {
    if (!boundedText(value.title, 8 * 1024)
      || !boundedText(value.markdown, 240 * 1024)
      || new TextEncoder().encode(value.title).length
        + new TextEncoder().encode(value.markdown).length
        + (value.summary === null ? 0 : new TextEncoder().encode(value.summary).length)
        > 240 * 1024) throw new Error('invalid_response');
  } else if (value.title !== null || value.markdown !== null) {
    throw new Error('invalid_response');
  }
  return value;
}

function translationStateText(state) {
  switch (state) {
    case 'not_requested': return '当前版本没有可用的翻译任务。';
    case 'queued': return '当前版本正在等待翻译。';
    case 'running': return '当前版本正在翻译。';
    case 'succeeded': return '当前版本翻译已通过校验。';
    case 'failed': return '当前版本翻译失败。';
    case 'unavailable': return '当前版本的翻译校验失败，未显示任何内容。';
    default: return '当前版本翻译状态不可用。';
  }
}

function summaryStateText(state) {
  switch (state) {
    case 'not_requested': return '当前版本没有可用的摘要任务。';
    case 'queued': return '当前版本正在等待生成摘要。';
    case 'running': return '当前版本正在生成摘要。';
    case 'succeeded': return '当前版本摘要已通过校验。';
    case 'failed': return '当前版本摘要生成失败。';
    case 'unavailable': return '当前版本的摘要校验失败，未显示任何内容。';
    default: return '当前版本摘要状态不可用。';
  }
}

function libraryMetadataText(item) {
  const itemType = item.item_type === 'webpage' ? '网页' : '资料';
  const details = [item.site_name, itemType, item.captured_at].filter(Boolean);
  return details.join(' · ');
}

function clearLibraryItem(document) {
  element('library-item', document).hidden = true;
  element('library-item-title', document).textContent = '';
  element('library-item-meta', document).textContent = '';
  element('library-item-summary-state', document).textContent = '';
  element('library-item-summary', document).textContent = '';
  element('library-item-body', document).textContent = '';
}

function renderLibraryItem(document, listed, item) {
  element('library-item-title', document).textContent = item.title ?? listed.title;
  element('library-item-meta', document).textContent = libraryMetadataText(listed);
  element('library-item-summary-state', document).textContent = summaryStateText(item.summary_state);
  element('library-item-summary', document).textContent = item.summary ?? '';
  element('library-item-body', document).textContent = item.markdown ?? '';
  element('library-item', document).hidden = false;
}

export function validateOutbox(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('storage_unavailable');
  const keys = Object.keys(value).sort().join(',');
  if (keys !== 'item_id,job_id,mac_device_id,mac_identity_public_key,request_key,state,url') {
    throw new Error('storage_unavailable');
  }
  if (!/^remote_web_[A-Za-z0-9_-]{32}$/.test(value.request_key)) throw new Error('storage_unavailable');
  if (validateCaptureUrl(value.url) !== value.url) throw new Error('storage_unavailable');
  if (value.job_id !== null && !JOB_PATTERN.test(value.job_id)) throw new Error('storage_unavailable');
  if (value.item_id !== null && !ITEM_PATTERN.test(value.item_id)) throw new Error('storage_unavailable');
  if (typeof value.state !== 'string' || value.state.length > 32) throw new Error('storage_unavailable');
  const unbound = value.mac_device_id === null && value.mac_identity_public_key === null;
  if (!unbound) {
    if (
      typeof value.mac_device_id !== 'string'
      || value.mac_device_id.length === 0
      || value.mac_device_id.length > 128
      || typeof value.mac_identity_public_key !== 'string'
      || !/^[A-Za-z0-9_-]{43}$/.test(value.mac_identity_public_key)
    ) throw new Error('storage_unavailable');
  } else if (value.state !== 'confirmation_required') {
    throw new Error('storage_unavailable');
  }
  return value;
}

export function normalizeStoredOutbox(value, pairing) {
  let candidate = value;
  if (
    candidate
    && typeof candidate === 'object'
    && !Array.isArray(candidate)
    && Object.keys(candidate).sort().join(',') === 'item_id,job_id,request_key,state,url'
  ) {
    candidate = {
      ...candidate,
      mac_device_id: null,
      mac_identity_public_key: null,
      state: 'confirmation_required',
    };
  }
  candidate = validateOutbox(candidate);
  if (
    candidate.mac_device_id !== pairing.mac_device_id
    || candidate.mac_identity_public_key !== pairing.mac_identity_public_key
  ) return { ...candidate, state: 'confirmation_required' };
  return candidate;
}

export function canResumeOutbox(outbox, pairing) {
  return outbox.state !== 'confirmation_required'
    && outbox.mac_device_id === pairing.mac_device_id
    && outbox.mac_identity_public_key === pairing.mac_identity_public_key;
}

export function outboxOperationIdentity(value) {
  return {
    request_key: value.request_key,
    url: value.url,
    mac_device_id: value.mac_device_id,
    mac_identity_public_key: value.mac_identity_public_key,
  };
}

function sameOutboxOperation(left, right) {
  return left.request_key === right.request_key
    && left.url === right.url
    && left.mac_device_id === right.mac_device_id
    && left.mac_identity_public_key === right.mac_identity_public_key;
}

export async function durableOutboxTransition({ database, current, next, workVersion }) {
  const validated = validateOutbox(next);
  await compareAndSetOutbox(database, current, validated);
  return { outbox: validated, workVersion: workVersion + 1 };
}

export function statusText(state) {
  switch (state) {
    case 'pending_delivery': return '正在等待 Mac 接收——尚未受理。';
    case 'queued': return 'Mac 已接收，正在排队。';
    case 'leased': return 'Mac 正在处理。';
    case 'artifact_ready': return '网页已采集，Mac 正在准备存储。';
    case 'retry_wait': return 'Mac 已接收，稍后将重试。';
    case 'needs_user': return '需要在 Mac 上批准。批准后请在此刷新状态。';
    case 'confirmation_required': return '这条已保存的采集属于另一台或身份未知的 Mac。请将其丢弃，或明确作为新请求提交给当前 Mac。';
    case 'stopping': return '正在停止本机检查并删除已保存的待处理操作…';
    case 'stop_error': return '无法可靠停止。请重试“仅停止本机检查”，或重新载入本页面。';
    case 'succeeded': return '存储成功，正在读取已授权结果…';
    case 'failed': return 'Mac 报告此次采集失败。';
    case 'cancelled': return 'Mac 报告此次采集已取消。';
    default: return '暂时无法获取状态。';
  }
}

function publicError(error) {
  return ERROR_MESSAGES[error?.message] || '无法完成安全请求。';
}

function withTimeout(promise, timeoutMs, onTimeout) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      onTimeout?.();
      reject(new Error('mac_offline'));
    }, timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

export function openSecureChannel({
  pairing,
  identity,
  phoneId,
  persistTrust,
  WebSocketValue = WebSocket,
  timeoutMs = RPC_TIMEOUT_MS,
  allowLocal = false,
}) {
  return new Promise((resolve, reject) => {
    let settled = false;
    let receiveChain = Promise.resolve();
    let session;
    const socket = new WebSocketValue(
      relaySocketUrl(pairing, { allowLocal }), browserSubprotocols(pairing),
    );
    const fail = (error = new Error('mac_offline')) => {
      session?.close();
      if (!settled) {
        settled = true;
        reject(error);
      }
    };
    const timer = setTimeout(() => {
      try { socket.close(4002, 'timeout'); } catch { /* already unavailable */ }
      fail();
    }, timeoutMs);
    session = new WebSecureSession({
      pairing,
      identity,
      phoneDeviceId: phoneId,
      send: (message) => socket.send(message),
      onTrust: persistTrust,
      onReady: async () => {
        if (socket.protocol !== FIXED_SUBPROTOCOL) throw new Error('invalid_message');
        clearTimeout(timer);
        settled = true;
        resolve({
          socket,
          // A caller may allow a longer wait for one request (item chat answers take minutes).
          request: (id, method, params, { timeoutMs: requestTimeoutMs = timeoutMs } = {}) => withTimeout(
            session.request(id, method, params),
            requestTimeoutMs,
            () => socket.close(4002, 'timeout'),
          ),
          close: () => {
            session.close();
            socket.close(1000, 'local_close');
          },
        });
      },
    });
    socket.addEventListener('open', () => {
      if (socket.protocol !== FIXED_SUBPROTOCOL) {
        socket.close(4000, 'protocol_error');
        fail(new Error('invalid_message'));
        return;
      }
      session.start().catch(fail);
    });
    socket.addEventListener('message', (event) => {
      receiveChain = receiveChain.then(() => session.receive(event.data)).catch((error) => {
        try { socket.close(4000, 'protocol_error'); } catch { /* already unavailable */ }
        fail(error);
      });
    });
    socket.addEventListener('close', () => fail());
    socket.addEventListener('error', () => fail());
  });
}

export class BrowserApp {
  constructor({
    fragment,
    hadQuery,
    pageHref,
    documentValue = document,
    acquireLease = acquireBrowserLease,
    capabilityProbe = runCapabilityProbe,
    databaseOpen = openDatabase,
    scanPairing = scanPairingCode,
    channelOpen = openSecureChannel,
    webClientUrl = WEB_CLIENT_URL,
    relayUrl = RELAY_URL,
    allowLocal = ALLOW_LOCAL,
  }) {
    this.fragment = fragment;
    this.hadQuery = hadQuery;
    this.pageHref = pageHref;
    this.document = documentValue;
    this.acquireLease = acquireLease;
    this.capabilityProbe = capabilityProbe;
    this.databaseOpen = databaseOpen;
    this.scanPairing = scanPairing;
    this.channelOpen = channelOpen;
    this.transportConfig = { webClientUrl, relayUrl, allowLocal };
    this.lease = null;
    this.database = null;
    this.identity = null;
    this.phoneId = null;
    this.pairing = null;
    this.outbox = null;
    this.outboxTransition = null;
    this.outboxWriteTail = Promise.resolve();
    this.stopFence = null;
    this.channel = null;
    this.connecting = false;
    this.connectTimer = null;
    this.pollTimer = null;
    this.polls = 0;
    this.processing = false;
    this.rpcSequence = 0;
    this.workVersion = 0;
    this.scanController = null;
    this.pairingEventsBound = false;
    this.eventsBound = false;
    this.libraryEventsBound = false;
    this.loadingLibrary = null;
    this.libraryReadGeneration = 0;
    this.items = [];
    this.nextCursor = null;
  }

  setConnection(label, className = '') {
    element('connection-state', this.document).textContent = label;
    element('connection-dot', this.document).className = `dot${className ? ` ${className}` : ''}`;
  }

  setError(error) {
    element('error-message', this.document).textContent = publicError(error);
    element('error-card', this.document).hidden = false;
  }

  clearError() {
    element('error-message', this.document).textContent = '';
    element('error-card', this.document).hidden = true;
  }

  renderOutbox() {
    const hasOutbox = Boolean(this.outbox);
    const fenced = Boolean(this.stopFence);
    const transitioning = Boolean(this.outboxTransition) || this.stopFence?.status === 'stopping';
    element('job', this.document).hidden = !hasOutbox;
    element('capture-url', this.document).disabled = hasOutbox || transitioning || fenced;
    element('submit-button', this.document).disabled = hasOutbox || transitioning || fenced;
    element('refresh-button', this.document).disabled = transitioning || fenced;
    element('reconfirm-button', this.document).disabled = transitioning || fenced;
    element('cancel-button', this.document).disabled = transitioning;
    if (hasOutbox) {
      const needsConfirmation = this.outbox.state === 'confirmation_required';
      element('job-status', this.document).textContent = statusText(this.outbox.state);
      element('refresh-button', this.document).hidden = needsConfirmation;
      element('reconfirm-button', this.document).hidden = !needsConfirmation;
      element('cancel-button', this.document).hidden = false;
    }
  }

  queueOutboxWrite(operation) {
    const completion = this.outboxWriteTail.then(operation);
    this.outboxWriteTail = completion.catch(() => {});
    return completion;
  }

  beginOutboxTransition(current, next) {
    if (this.outboxTransition) throw new Error('pending_exists');
    // The CAS becomes sendable only if this exact memory epoch survives until commit;
    // Stop advances the epoch and conditionally removes only this candidate instead.
    const transition = {
      current,
      currentRequestKey: current?.request_key ?? null,
      currentState: current?.state ?? null,
      next,
      version: this.workVersion,
      completion: null,
    };
    this.outboxTransition = transition;
    this.renderOutbox();
    transition.completion = this.queueOutboxWrite(() => durableOutboxTransition({
      database: this.database,
      current,
      next,
      workVersion: transition.version,
    }));
    return transition;
  }

  canPublishOutboxTransition(transition) {
    return this.outboxTransition === transition
      && !this.stopFence
      && this.workVersion === transition.version
      && this.outbox === transition.current
      && (transition.current === null || (
        this.outbox?.request_key === transition.currentRequestKey
        && this.outbox?.state === transition.currentState
      ));
  }

  finishOutboxTransition(transition) {
    if (this.outboxTransition !== transition) return;
    this.outboxTransition = null;
    this.renderOutbox();
  }

  showUnsupported(error) {
    const anotherTab = error?.message === 'another_tab_active';
    this.setConnection(anotherTab ? '另一标签页正在运行' : '不受支持');
    element('unsupported-heading', this.document).textContent = anotherTab
      ? '另一标签页正在运行'
      : '浏览器不受支持';
    element('unsupported-message', this.document).textContent = publicError(error);
    element('unsupported', this.document).hidden = false;
    if (optionalElement('unpaired', this.document)) element('unpaired', this.document).hidden = true;
    if (optionalElement('paired', this.document)) element('paired', this.document).hidden = true;
    if (optionalElement('pairing-scanner', this.document)) element('pairing-scanner', this.document).hidden = true;
    if (optionalElement('capture', this.document)) element('capture', this.document).hidden = true;
    if (optionalElement('job', this.document)) element('job', this.document).hidden = true;
  }

  async initialize() {
    if (this.hadQuery || this.fragment) throw new Error('invalid_navigation');
    validateSettingsPage(this.pageHref, this.transportConfig.webClientUrl);
    this.bindProductEvents();
    this.lease = await this.acquireLease();
    try {
      await this.capabilityProbe();
      this.database = await this.databaseOpen();
      this.identity = await loadOrCreateIdentity(this.database);
      this.phoneId = await loadOrCreatePhoneId(this.database);

      const stored = await getState(this.database, 'pairing');
      if (stored?.trusted === false) {
        await deleteState(this.database, 'pairing');
      } else if (stored) {
        this.pairing = validatePersistedPairing(
          stored, this.pageHref, this.transportConfig,
        );
      }

      if (!this.pairing) {
        this.setConnection('尚未连接设备');
        if (optionalElement('library-state', this.document)) {
          this.setLibraryState('尚未连接 Mac。请在“设置”中完成设备连接。');
        }
        element('unpaired', this.document).hidden = false;
        if (optionalElement('paired', this.document)) element('paired', this.document).hidden = true;
        this.bindPairingEvents();
        return;
      }
      await this.activatePairing();
    } catch (error) {
      await this.lease.release();
      this.lease = null;
      throw error;
    }
  }

  bindPairingEvents() {
    if (this.pairingEventsBound) return;
    this.pairingEventsBound = true;
    element('scan-pairing-button', this.document).addEventListener('click', () => {
      this.beginPairingScan().catch((error) => this.setError(error));
    });
    element('cancel-pairing-button', this.document).addEventListener('click', () => {
      this.scanController?.abort();
    });
  }

  bindProductEvents() {
    optionalElement('library-refresh', this.document)?.addEventListener('click', () => this.loadLibrary(true));
    optionalElement('library-more', this.document)?.addEventListener('click', () => this.loadLibrary(false));
  }

  setLibraryState(message) {
    element('library-state', this.document).textContent = message;
  }

  resetLibrary(message) {
    this.loadingLibrary = null;
    this.libraryReadGeneration += 1;
    this.items = [];
    this.nextCursor = null;
    optionalElement('library-list', this.document)?.replaceChildren();
    if (optionalElement('library-more', this.document)) element('library-more', this.document).hidden = true;
    if (optionalElement('library-item', this.document)) clearLibraryItem(this.document);
    if (optionalElement('library-state', this.document)) this.setLibraryState(message);
  }

  async beginPairingScan() {
    if (this.pairing || this.scanController) return;
    const controller = new AbortController();
    this.scanController = controller;
    element('scan-pairing-button', this.document).disabled = true;
    element('unpaired', this.document).hidden = true;
    if (optionalElement('paired', this.document)) element('paired', this.document).hidden = true;
    element('pairing-scanner', this.document).hidden = false;
    element('pairing-scan-status', this.document).textContent = '请将 Mac 小窗口中的二维码放入取景框。';
    this.clearError();
    this.setConnection('正在使用相机…');
    try {
      const code = await this.scanPairing({
        video: element('pairing-video', this.document),
        canvas: element('pairing-canvas', this.document),
        signal: controller.signal,
      });
      const pairing = parsePairingCode(code, this.pageHref, this.transportConfig);
      this.pairing = pairing;
      element('pairing-scanner', this.document).hidden = true;
      await this.activatePairing();
    } catch (error) {
      if (error?.message !== 'scan_cancelled') this.setError(error);
      if (!this.pairing) {
        this.setConnection('尚未连接设备');
        element('unpaired', this.document).hidden = false;
        if (optionalElement('paired', this.document)) element('paired', this.document).hidden = true;
      }
    } finally {
      if (this.scanController === controller) this.scanController = null;
      element('scan-pairing-button', this.document).disabled = false;
      element('pairing-scanner', this.document).hidden = true;
    }
  }

  async activatePairing() {
      const storedOutbox = await getState(this.database, 'outbox');
      if (storedOutbox) {
        const normalized = normalizeStoredOutbox(storedOutbox, this.pairing);
        await this.queueOutboxWrite(() => compareAndSetOutbox(
          this.database, storedOutbox, normalized,
        ));
        this.outbox = normalized;
      }
      element('capture', this.document).hidden = !this.pairing.trusted;
      element('unpaired', this.document).hidden = true;
      if (optionalElement('paired', this.document)) {
        element('paired', this.document).hidden = !this.pairing.trusted;
      }
      this.renderOutbox();
      this.bindEvents();
      this.setConnection(this.pairing.trusted ? 'Mac 离线' : '正在连接 Mac', 'offline');
      this.scheduleConnect(0);
  }

  bindEvents() {
    if (this.eventsBound) return;
    this.eventsBound = true;
    element('capture-form', this.document).addEventListener('submit', (event) => {
      event.preventDefault();
      this.submit(element('capture-url', this.document).value).catch((error) => this.setError(error));
    });
    element('refresh-button', this.document).addEventListener('click', () => {
      this.polls = 0;
      this.processOutbox().catch((error) => this.setError(error));
    });
    element('reconfirm-button', this.document).addEventListener('click', () => {
      this.reconfirmForCurrentMac().catch((error) => this.setError(error));
    });
    element('cancel-button', this.document).addEventListener('click', () => {
      this.stopLocally().catch((error) => this.setError(error));
    });
  }

  scheduleConnect(delay) {
    clearTimeout(this.connectTimer);
    this.connectTimer = setTimeout(() => this.connect().catch(() => {}), delay);
  }

  async connect() {
    if (this.connecting || this.channel || !this.pairing) return;
    this.connecting = true;
    try {
      const channel = await this.channelOpen({
        pairing: this.pairing,
        identity: this.identity,
        phoneId: this.phoneId,
        persistTrust: async (trusted) => {
          await putState(this.database, 'pairing', trusted);
          this.pairing = trusted;
        },
        allowLocal: this.transportConfig.allowLocal,
      });
      this.channel = channel;
      channel.socket.addEventListener('close', () => {
        if (this.channel !== channel) return;
        this.channel = null;
        this.setConnection('Mac 离线', 'offline');
        if (optionalElement('library-refresh', this.document)) {
          element('library-refresh', this.document).disabled = true;
          this.resetLibrary('Mac 离线，资料库将在重新连接后加载。');
        }
        this.scheduleConnect(3_000);
      });
      element('capture', this.document).hidden = false;
      this.renderOutbox();
      this.clearError();
      this.setConnection('Mac 在线', 'online');
      if (optionalElement('paired', this.document)) element('paired', this.document).hidden = false;
      if (optionalElement('library-refresh', this.document)) {
        element('library-refresh', this.document).disabled = false;
        await this.loadLibrary(true);
      }
      await this.processOutbox();
    } catch {
      if (!this.pairing.trusted && this.pairing.expires_at_ms <= Date.now()) {
        this.pairing = null;
        element('capture', this.document).hidden = true;
        element('job', this.document).hidden = true;
        element('unpaired', this.document).hidden = false;
        if (optionalElement('paired', this.document)) element('paired', this.document).hidden = true;
        this.setConnection('尚未连接设备');
        return;
      }
      this.setConnection(this.pairing.trusted ? 'Mac 离线' : '正在连接 Mac', 'offline');
      this.scheduleConnect(3_000);
    } finally {
      this.connecting = false;
    }
  }

  async submit(rawUrl) {
    if (!this.pairing?.trusted) throw new Error('mac_offline');
    if (this.outbox || this.outboxTransition || this.stopFence) throw new Error('pending_exists');
    const url = validateCaptureUrl(rawUrl.trim());
    const next = {
      request_key: `remote_web_${base64url(crypto.getRandomValues(new Uint8Array(24)))}`,
      url,
      job_id: null,
      item_id: null,
      mac_device_id: this.pairing.mac_device_id,
      mac_identity_public_key: this.pairing.mac_identity_public_key,
      state: 'pending_delivery',
    };
    const transition = this.beginOutboxTransition(null, next);
    let committed;
    try {
      committed = await transition.completion;
    } catch (error) {
      if (!this.canPublishOutboxTransition(transition)) return;
      this.finishOutboxTransition(transition);
      throw error;
    }
    if (!this.canPublishOutboxTransition(transition)) return;
    this.outbox = committed.outbox;
    this.workVersion = committed.workVersion;
    this.finishOutboxTransition(transition);
    this.polls = 0;
    this.clearError();
    element('result', this.document).hidden = true;
    element('result-title', this.document).textContent = '';
    element('result-body', this.document).textContent = '';
    element('library-link', this.document).hidden = true;
    this.renderOutbox();
    if (this.channel) await this.processOutbox();
  }

  renderLibraryItems() {
    const list = element('library-list', this.document);
    list.replaceChildren();
    for (const item of this.items) {
      const button = this.document.createElement('button');
      button.type = 'button';
      button.className = 'library-item-button';
      const title = this.document.createElement('span');
      title.className = 'library-item-title';
      title.textContent = item.title;
      const metadata = this.document.createElement('span');
      metadata.className = 'library-item-meta';
      metadata.textContent = libraryMetadataText(item);
      button.append(title, metadata);
      button.addEventListener('click', () => this.readLibraryItem(item).catch((error) => this.setError(error)));
      list.append(button);
    }
  }

  async loadLibrary(reset) {
    const channel = this.channel;
    if (!channel || this.loadingLibrary?.channel === channel || (!reset && !this.nextCursor)) return;
    const loading = { channel };
    this.loadingLibrary = loading;
    if (reset) {
      this.libraryReadGeneration += 1;
      clearLibraryItem(this.document);
    }
    this.setLibraryState(reset ? '正在从 Mac Store 读取…' : '正在加载更多…');
    try {
      const result = validateLibraryListResult(await this.rpc('memex.library.list', {
        cursor: reset ? null : this.nextCursor,
      }));
      if (this.channel !== channel || this.loadingLibrary !== loading) return;
      this.items = reset ? [...result.items] : [...this.items, ...result.items];
      this.nextCursor = result.next_cursor;
      this.renderLibraryItems();
      element('library-more', this.document).hidden = this.nextCursor === null;
      this.setLibraryState(this.items.length ? `已读取 ${this.items.length} 条资料。` : '资料库为空。');
    } catch (error) {
      if (this.channel === channel && this.loadingLibrary === loading) {
        if (reset) this.resetLibrary('加载失败。请确认 Mac 在线后重试。');
        else this.setLibraryState('加载失败。请确认 Mac 在线后重试。');
        this.setError(error);
      }
    } finally {
      if (this.loadingLibrary === loading) this.loadingLibrary = null;
    }
  }

  async readLibraryItem(summary) {
    const channel = this.channel;
    if (!channel) throw new Error('mac_offline');
    const generation = ++this.libraryReadGeneration;
    clearLibraryItem(this.document);
    this.setLibraryState('正在读取正文…');
    let item;
    try {
      item = validateLibraryItemResult(await this.rpc(
        'memex.library.item.read', { item_id: summary.item_id },
      ), summary.item_id);
    } catch (error) {
      if (this.channel !== channel || generation !== this.libraryReadGeneration) return;
      throw error;
    }
    if (this.channel !== channel || generation !== this.libraryReadGeneration) return;
    renderLibraryItem(this.document, summary, item);
    this.setLibraryState(`${summaryStateText(item.summary_state)} ${translationStateText(item.translation_state)}`);
  }

  async rpc(method, params) {
    if (!this.channel) throw new Error('mac_offline');
    const response = await this.channel.request(`rpc_web_${++this.rpcSequence}`, method, params);
    if (!response || typeof response !== 'object' || Array.isArray(response)) throw new Error('invalid_response');
    if (response.error) throw new Error(typeof response.error.code === 'string' ? response.error.code : 'invalid_response');
    if (!response.result || typeof response.result !== 'object' || Array.isArray(response.result)) {
      throw new Error('invalid_response');
    }
    return response.result;
  }

  async saveOutbox(next, requestKey, version) {
    if (
      this.stopFence
      || this.workVersion !== version
      || this.outbox?.request_key !== requestKey
    ) return false;
    const current = this.outbox;
    const validated = validateOutbox(next);
    if (!sameOutboxOperation(
      outboxOperationIdentity(current), outboxOperationIdentity(validated),
    )) throw new Error('storage_unavailable');
    await this.queueOutboxWrite(() => compareAndSetOutbox(this.database, current, validated));
    if (
      this.stopFence
      || this.workVersion !== version
      || this.outbox !== current
      || this.outbox?.request_key !== requestKey
    ) return false;
    this.outbox = validated;
    this.renderOutbox();
    return true;
  }

  async processOutbox() {
    if (
      this.processing || this.stopFence || this.outboxTransition
      || !this.outbox || !this.channel
    ) return;
    if (!canResumeOutbox(this.outbox, this.pairing)) return;
    this.processing = true;
    clearTimeout(this.pollTimer);
    const version = this.workVersion;
    const requestKey = this.outbox.request_key;
    try {
      let job = this.outbox.job_id
        ? await this.rpc('memex.capture.status', { job_id: this.outbox.job_id })
        : await this.rpc('memex.capture.enqueue', {
          request_key: this.outbox.request_key,
          url: this.outbox.url,
        });
      if (!JOB_PATTERN.test(job.job_id) || typeof job.state !== 'string') throw new Error('invalid_response');
      if (!await this.saveOutbox({
        ...this.outbox,
        job_id: job.job_id,
        item_id: typeof job.item_id === 'string' ? job.item_id : null,
        state: job.state,
      }, requestKey, version)) return;
      this.clearError();

      if (job.state === 'succeeded') {
        if (!ITEM_PATTERN.test(job.item_id)) throw new Error('invalid_response');
        const item = await this.rpc('memex.item.read', { job_id: job.job_id, item_id: job.item_id });
        if (
          item.item_id !== job.item_id
          || typeof item.title !== 'string'
          || typeof item.markdown !== 'string'
        ) throw new Error('invalid_response');
        if (this.workVersion !== version || this.outbox?.request_key !== requestKey) return;
        element('result-title', this.document).textContent = item.title;
        element('result-body', this.document).textContent = item.markdown;
        element('library-link', this.document).hidden = false;
        element('result', this.document).hidden = false;
        await this.finishOutbox('存储成功。');
        return;
      }
      if (TERMINAL_STATES.has(job.state)) {
        await this.finishOutbox(statusText(job.state));
        return;
      }
      if (job.state === 'needs_user') return;
      if (!RUNNING_STATES.has(job.state)) throw new Error('invalid_response');
      if (this.polls < MAX_AUTO_POLLS) {
        this.polls += 1;
        this.pollTimer = setTimeout(() => this.processOutbox().catch((error) => this.setError(error)), POLL_INTERVAL_MS);
      } else {
        element('job-status', this.document).textContent = 'Mac 仍在处理。请点按“刷新状态”再次检查。';
      }
    } catch (error) {
      this.setError(error);
      if (error.message === 'mac_offline') {
        this.channel = null;
        this.setConnection('Mac 离线', 'offline');
        this.scheduleConnect(3_000);
      }
    } finally {
      this.processing = false;
    }
  }

  async finishOutbox(message) {
    const current = this.outbox;
    if (!current || this.stopFence) return;
    const version = this.workVersion;
    const identity = outboxOperationIdentity(current);
    let result;
    try {
      result = await this.queueOutboxWrite(() => deleteOutboxByIdentity(
        this.database, identity,
      ));
    } catch (error) {
      if (this.workVersion === version && this.outbox === current && !this.stopFence) {
        const fence = { status: 'error', identity, epoch: ++this.workVersion };
        this.stopFence = fence;
        await this.showStopError(fence, current);
      }
      throw error;
    }
    if (this.stopFence || this.workVersion !== version || this.outbox !== current) return;
    if (!result.deleted && result.current) {
      const fence = { status: 'error', identity, epoch: ++this.workVersion };
      this.stopFence = fence;
      await this.showStopError(fence, result.current);
      throw new Error('outbox_conflict');
    }
    this.workVersion += 1;
    this.outbox = null;
    this.renderOutbox();
    element('job', this.document).hidden = false;
    element('job-status', this.document).textContent = message;
    element('refresh-button', this.document).hidden = true;
    element('reconfirm-button', this.document).hidden = true;
    element('cancel-button', this.document).hidden = true;
  }

  async reconfirmForCurrentMac() {
    if (!this.outbox || this.outbox.state !== 'confirmation_required') return;
    const current = this.outbox;
    const next = {
      ...this.outbox,
      request_key: `remote_web_${base64url(crypto.getRandomValues(new Uint8Array(24)))}`,
      job_id: null,
      item_id: null,
      mac_device_id: this.pairing.mac_device_id,
      mac_identity_public_key: this.pairing.mac_identity_public_key,
      state: 'pending_delivery',
    };
    const transition = this.beginOutboxTransition(current, next);
    let committed;
    try {
      committed = await transition.completion;
    } catch (error) {
      if (!this.canPublishOutboxTransition(transition)) return;
      this.finishOutboxTransition(transition);
      throw error;
    }
    if (!this.canPublishOutboxTransition(transition)) return;
    this.outbox = committed.outbox;
    this.workVersion = committed.workVersion;
    this.finishOutboxTransition(transition);
    this.polls = 0;
    this.clearError();
    this.renderOutbox();
    if (this.channel) await this.processOutbox();
  }

  async showStopError(fence, fallback) {
    let durable;
    try {
      const stored = await getState(this.database, 'outbox');
      if (stored) durable = validateOutbox(stored);
    } catch {
      // Keep the known operation visible when durable reload is unavailable.
    }
    if (this.stopFence !== fence) return;
    const visible = durable || fallback;
    this.outbox = visible ? { ...visible, state: 'stop_error' } : null;
    fence.status = 'error';
    if (this.outboxTransition) this.finishOutboxTransition(this.outboxTransition);
    this.renderOutbox();
  }

  async stopLocally() {
    if (this.stopFence?.status === 'stopping') return;
    const priorFence = this.stopFence?.status === 'error' ? this.stopFence : null;
    const transition = this.outboxTransition;
    const current = this.outbox;
    if (!current && !transition && !priorFence) return;
    let target = transition?.next || current;
    let identity = priorFence?.identity || outboxOperationIdentity(target);
    const fence = { status: 'stopping', identity, epoch: ++this.workVersion };
    this.stopFence = fence;
    clearTimeout(this.pollTimer);
    this.outbox = target ? { ...target, state: 'stopping' } : null;
    this.renderOutbox();
    if (transition && !priorFence) {
      try {
        await transition.completion;
        target = transition.next;
      } catch {
        target = transition.current || transition.next;
      }
      identity = outboxOperationIdentity(target);
      fence.identity = identity;
    }
    if (transition) this.finishOutboxTransition(transition);
    let result;
    try {
      result = await this.queueOutboxWrite(() => deleteOutboxByIdentity(
        this.database, identity,
      ));
    } catch (error) {
      await this.showStopError(fence, target);
      throw error;
    }
    if (this.stopFence !== fence) return;
    if (!result.deleted && result.current) {
      await this.showStopError(fence, result.current);
      throw new Error('outbox_conflict');
    }
    this.stopFence = null;
    this.outbox = null;
    this.clearError();
    this.renderOutbox();
    element('job', this.document).hidden = false;
    element('job-status', this.document).textContent = '已停止本机检查。Mac 上的任务并未取消。';
    element('refresh-button', this.document).hidden = true;
    element('reconfirm-button', this.document).hidden = true;
    element('cancel-button', this.document).hidden = true;
  }
}

export class LibraryApp {
  constructor({
    pageHref,
    documentValue = document,
    acquireLease = acquireBrowserLease,
    capabilityProbe = runCapabilityProbe,
    databaseOpen = openDatabase,
    channelOpen = openSecureChannel,
    webClientUrl = WEB_CLIENT_URL,
    relayUrl = RELAY_URL,
    allowLocal = ALLOW_LOCAL,
  }) {
    this.pageHref = pageHref;
    this.document = documentValue;
    this.acquireLease = acquireLease;
    this.capabilityProbe = capabilityProbe;
    this.databaseOpen = databaseOpen;
    this.channelOpen = channelOpen;
    this.transportConfig = { webClientUrl, relayUrl, allowLocal };
    this.lease = null;
    this.database = null;
    this.identity = null;
    this.phoneId = null;
    this.pairing = null;
    this.channel = null;
    this.connecting = false;
    this.connectTimer = null;
    this.loading = null;
    this.rpcSequence = 0;
    this.readGeneration = 0;
    this.items = [];
    this.nextCursor = null;
    this.searchText = '';
    this.typeFilter = 'all';
    this.eventsBound = false;
  }

  setConnection(label, className = '') {
    element('connection-state', this.document).textContent = label;
    element('connection-dot', this.document).className = `dot${className ? ` ${className}` : ''}`;
  }

  setState(message) {
    element('library-state', this.document).textContent = message;
  }

  setCount(message) {
    element('library-count', this.document).textContent = message;
  }

  setError(error) {
    element('error-message', this.document).textContent = publicError(error);
    element('error-card', this.document).hidden = false;
  }

  clearError() {
    element('error-message', this.document).textContent = '';
    element('error-card', this.document).hidden = true;
  }

  showUnsupported(error) {
    const anotherTab = error?.message === 'another_tab_active';
    this.setConnection(anotherTab ? '另一标签页正在运行' : '不受支持');
    element('unsupported-heading', this.document).textContent = anotherTab
      ? '另一标签页正在运行'
      : '无法打开实时资料库';
    element('unsupported-message', this.document).textContent = publicError(error);
    element('unsupported', this.document).hidden = false;
    element('unpaired', this.document).hidden = false;
    element('library', this.document).hidden = false;
    this.setState('实时 Mac Store 当前不可用，未显示权威条目数。');
    this.setCount('不可用');
  }

  async initialize() {
    validateLibraryPage(this.pageHref, this.transportConfig.webClientUrl);
    this.lease = await this.acquireLease();
    try {
      this.bindEvents();
      await this.capabilityProbe();
      this.database = await this.databaseOpen();
      const stored = await getState(this.database, 'pairing');
      if (!stored) {
        this.setConnection('尚未连接设备');
        element('unpaired', this.document).hidden = false;
        this.setState('尚未连接 Mac，资料库当前不可用。');
        this.setCount('不可用');
        return;
      }
      this.pairing = validatePersistedPairing(
        stored,
        this.transportConfig.webClientUrl,
        this.transportConfig,
      );
      if (!this.pairing.trusted) {
        this.pairing = null;
        this.setConnection('尚未连接设备');
        element('unpaired', this.document).hidden = false;
        this.setState('连接信任不可用，未显示权威条目数。');
        this.setCount('不可用');
        return;
      }
      this.identity = await loadOrCreateIdentity(this.database);
      this.phoneId = await loadOrCreatePhoneId(this.database);
      element('library', this.document).hidden = false;
      element('unpaired', this.document).hidden = true;
      this.setConnection('Mac 离线', 'offline');
      this.setState('Mac 离线，当前没有权威资料可显示。');
      this.setCount('不可用');
      this.scheduleConnect(0);
    } catch (error) {
      await this.lease.release();
      this.lease = null;
      throw error;
    }
  }

  bindEvents() {
    if (this.eventsBound) return;
    this.eventsBound = true;
    element('library-refresh', this.document).addEventListener('click', () => {
      this.loadPage(true).catch((error) => this.setError(error));
    });
    element('library-more', this.document).addEventListener('click', () => {
      this.loadPage(false).catch((error) => this.setError(error));
    });
    element('library-search', this.document).addEventListener('input', (event) => {
      this.searchText = String(event?.target?.value ?? element('library-search', this.document).value)
        .trim().toLocaleLowerCase();
      this.renderItems();
    });
    element('library-filter', this.document).addEventListener('change', (event) => {
      this.typeFilter = String(event?.target?.value ?? element('library-filter', this.document).value);
      this.renderItems();
    });
    element('theme-button', this.document).addEventListener('click', () => {
      const body = this.document.body;
      if (body) body.dataset.theme = body.dataset.theme === 'dark' ? 'light' : 'dark';
    });
    element('reader-back', this.document).addEventListener('click', () => {
      this.readGeneration += 1;
      clearLibraryItem(this.document);
      element('library-view', this.document).hidden = false;
    });
  }

  scheduleConnect(delay) {
    clearTimeout(this.connectTimer);
    this.connectTimer = setTimeout(() => this.connect().catch(() => {}), delay);
  }

  resetVisibleLibrary(message) {
    this.loading = null;
    this.readGeneration += 1;
    this.items = [];
    this.nextCursor = null;
    element('library-list', this.document).replaceChildren();
    element('library-more', this.document).hidden = true;
    clearLibraryItem(this.document);
    this.setState(message);
    this.setCount('不可用');
  }

  async connect() {
    if (this.connecting || this.channel || !this.pairing) return;
    this.connecting = true;
    try {
      const channel = await this.channelOpen({
        pairing: this.pairing,
        identity: this.identity,
        phoneId: this.phoneId,
        persistTrust: async (trusted) => {
          await putState(this.database, 'pairing', trusted);
          this.pairing = trusted;
        },
        allowLocal: this.transportConfig.allowLocal,
      });
      this.channel = channel;
      channel.socket.addEventListener('close', () => {
        if (this.channel !== channel) return;
        this.channel = null;
        this.setConnection('Mac 离线', 'offline');
        this.resetVisibleLibrary('Mac 离线，当前没有权威资料可显示。');
        this.scheduleConnect(3_000);
      });
      this.clearError();
      this.setConnection('Mac 在线', 'online');
      await this.loadPage(true);
    } catch (error) {
      if (!this.channel) {
        this.setConnection('Mac 离线', 'offline');
        this.resetVisibleLibrary('Mac 离线，当前没有权威资料可显示。');
        this.scheduleConnect(3_000);
      } else {
        this.setError(error);
      }
    } finally {
      this.connecting = false;
    }
  }

  async rpc(channel, method, params) {
    if (!channel || this.channel !== channel) throw new Error('mac_offline');
    const response = await channel.request(`rpc_library_${++this.rpcSequence}`, method, params);
    if (!response || typeof response !== 'object' || Array.isArray(response)) {
      throw new Error('invalid_response');
    }
    if (response.error) {
      throw new Error(typeof response.error.code === 'string'
        ? response.error.code : 'invalid_response');
    }
    if (!response.result || typeof response.result !== 'object' || Array.isArray(response.result)) {
      throw new Error('invalid_response');
    }
    return response.result;
  }

  renderItems() {
    const list = element('library-list', this.document);
    list.replaceChildren();
    const visible = this.items.filter((item) => {
      const matchesType = this.typeFilter === 'all'
        || (this.typeFilter === 'other' ? item.item_type !== 'webpage' : item.item_type === this.typeFilter);
      const text = `${item.title}\n${item.site_name || ''}`.toLocaleLowerCase();
      return matchesType && (!this.searchText || text.includes(this.searchText));
    });
    for (const item of visible) {
      const button = this.document.createElement('button');
      button.type = 'button';
      button.className = 'library-item-button';
      const title = this.document.createElement('span');
      title.className = 'library-item-title';
      title.textContent = item.title;
      const metadata = this.document.createElement('span');
      metadata.className = 'library-item-meta';
      metadata.textContent = libraryMetadataText(item);
      button.append(title, metadata);
      button.addEventListener('click', () => {
        this.readItem(item).catch((error) => this.setError(error));
      });
      list.append(button);
    }
    this.setCount(`${visible.length}${this.nextCursor === null ? '' : '+'}`);
  }

  async loadPage(reset) {
    const channel = this.channel;
    if (!channel || this.loading?.channel === channel) return;
    if (!reset && !this.nextCursor) return;
    const loading = { channel };
    this.loading = loading;
    if (reset) {
      this.readGeneration += 1;
      clearLibraryItem(this.document);
      element('library-view', this.document).hidden = false;
    }
    element('library-refresh', this.document).disabled = true;
    element('library-more', this.document).disabled = true;
    this.setState(reset ? '正在从 Mac Store 读取…' : '正在加载更多…');
    try {
      const result = validateLibraryListResult(await this.rpc(
        channel,
        'memex.library.list',
        { cursor: reset ? null : this.nextCursor },
      ));
      if (this.channel !== channel || this.loading !== loading) return;
      this.items = reset ? [...result.items] : [...this.items, ...result.items];
      this.nextCursor = result.next_cursor;
      this.renderItems();
      element('library-more', this.document).hidden = this.nextCursor === null;
      this.setState(this.items.length === 0
        ? 'Mac Store 中没有可显示的资料。'
        : `已从 Mac Store 读取 ${this.items.length} 条资料。`);
      this.clearError();
    } catch (error) {
      if (this.channel === channel && this.loading === loading) {
        if (reset) {
          this.items = [];
          this.nextCursor = null;
          this.renderItems();
          element('library-more', this.document).hidden = true;
          this.setCount('不可用');
          this.setState('加载失败，Mac Store 当前不可用，未显示权威条目数。');
        } else {
          this.setCount('可能过时');
          this.setState('加载更多失败；当前列表来自先前读取，可能不完整或过时。');
        }
        this.setError(error);
      }
    } finally {
      if (this.loading === loading) {
        this.loading = null;
        element('library-refresh', this.document).disabled = false;
        element('library-more', this.document).disabled = false;
      }
    }
  }

  async readItem(summary) {
    const channel = this.channel;
    if (!channel) throw new Error('mac_offline');
    const generation = ++this.readGeneration;
    this.clearError();
    clearLibraryItem(this.document);
    this.setState('正在从 Mac Store 读取正文…');
    try {
      const item = validateLibraryItemResult(await this.rpc(
        channel,
        'memex.library.item.read',
        { item_id: summary.item_id },
      ), summary.item_id);
      if (this.channel !== channel || generation !== this.readGeneration) return;
      renderLibraryItem(this.document, summary, item);
      element('library-view', this.document).hidden = true;
      this.setState(`${summaryStateText(item.summary_state)} ${translationStateText(item.translation_state)}`);
    } catch (error) {
      if (this.channel !== channel || generation !== this.readGeneration) return;
      this.setState('正文加载失败。请刷新资料库后重试。');
      throw error;
    }
  }
}

export async function start(options) {
  const app = new BrowserApp(options);
  try {
    await app.initialize();
  } catch (error) {
    app.showUnsupported(error);
  }
}

export async function startLibrary(options) {
  const app = new LibraryApp(options);
  try {
    await app.initialize();
  } catch (error) {
    app.showUnsupported(error);
  }
}
