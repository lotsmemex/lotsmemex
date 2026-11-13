// MemeX mobile web (m1/m2). Presentation only: data comes from a source adapter.
// Served by the Mac gateway at /mobile/index.html it uses the encrypted gateway source and never
// falls back to demo data; served anywhere else (the review preview) it uses synthetic demo data.

import { createDemoSource } from './demo-source.mjs';
import { createGatewaySource, openConnection } from './gateway-source.mjs';
import { SAMPLE_CHAT } from './fixtures.mjs';
import { langPref, setLangPref, tx, uiLang } from './i18n.mjs';
import { ASSISTANT_NAME, BRAND_NAME } from './brand.mjs';

document.title = BRAND_NAME;
document.querySelector('meta[name="apple-mobile-web-app-title"]')?.setAttribute('content', BRAND_NAME);
const GATEWAY = location.pathname === '/mobile/index.html';
let conn = null;
let source = GATEWAY ? null : createDemoSource();
const app = document.getElementById('app');
const PAGE = 80;

const state = {
  paired: !GATEWAY && localStorage.getItem('memex.m.paired') === '1',
  bootError: null,
  theme: localStorage.getItem('memex.m.theme') || 'system',
  readingFont: localStorage.getItem('memex.m.readingFont') === 'serif' ? 'serif' : 'sans',
  readingSize: [16, 18, 20, 22].includes(Number(localStorage.getItem('memex.m.readingSize'))) ? Number(localStorage.getItem('memex.m.readingSize')) : 18,
  // 设置 › 外观 › 注释字体: kai (楷体, default) | song (宋体) | body (与正文相同).
  // 设置 › 外观 › 说话人名字: orig (原文名, default) | trans (中文参考名) | both (原文名（中文名）). Display only.
  spkName: ['trans', 'both'].includes(localStorage.getItem('memex.m.spkName')) ? localStorage.getItem('memex.m.spkName') : 'orig',
  noteFont: ['song', 'body'].includes(localStorage.getItem('memex.m.noteFont')) ? localStorage.getItem('memex.m.noteFont') : 'kai',
  // A book's 对照: 'para' (paragraph-interleaved, default) or 'side' (two synced panes).
  bookLayout: localStorage.getItem('memex.m.bookLayout') === 'side' ? 'side' : 'para',
  // 对比修改: 'unified' (one stacked list, default) or 'side' (旧版 / 新版 halves, as the book's 并排).
  diffLayout: localStorage.getItem('memex.m.diffLayout') === 'side' ? 'side' : 'unified',
  online: true, // last request outcome; `unavailable` flips it, any success restores it
  reconnecting: 0, // foreground reads waiting for the channel to come back (replayed unchanged)
  lib: {
    // Show every origin by default; capture/import filters are explicit choices.
    // Entering a folder clears the previous location's origin filter.
    query: { text: '', source: null, item_type: null, sort: localStorage.getItem('memex.m.sort') || 'captured_at', descending: !['title', 'source'].includes(localStorage.getItem('memex.m.sort')),
      group_id: null, include_descendants: true, groupName: null, origin: localStorage.getItem('memex.m.origin') === 'capture' ? 'capture' : localStorage.getItem('memex.m.origin') === 'import' ? 'import' : null },
    view: null, rows: [], total: 0, next: 0, sources: [], item_types: [], loading: false, error: null, scroll: 0,
  },
  item: null, // { id, meta, text, nextByte, total, loading, error, translation }
  lang: 'orig', // reader mode: orig (原文) | bi (对照) | trans (译文)
  insView: 'text', // reader body: text | quotes (金句) | facts (事实核查)
  emphasis: false, // 重点标注 on the original; one choice for the session (desktop)
  manualOn: true, // 手工标记 shown (重点 menu: 都显示 / 只看 AI / 只看手工 / 都不显示 = emphasis × manualOn)
  side: null, // open sidebar: 'left' (library navigation) | 'right' (item details)
  // The reader's AI chat sheet starts folded (more reading room); the reader's last choice is kept.
  drawerOpen: localStorage.getItem('memex.m.aiFolded') === '0',
  chatDraft: '',
  agentDraft: '',
  tasks: [], // link captures sent from the 对话 tab; restored in boot()
  suggest: true,
  sheet: null, // { id, revision, draft, saving, conflict }
};

// ---------- tiny DOM helpers ----------
function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'html') el.innerHTML = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) el.append(kid.nodeType ? kid : String(kid));
  return el;
}
const ICONS = {
  search: '<circle cx="8" cy="8" r="5.5"/><path d="m12.2 12.2 3.3 3.3"/>',
  doc: '<path d="M5 2.5h6.5L15 6v11.5H5z"/><path d="M11 2.5V6.5h4M8 10h5M8 13h5"/>',
  chev: '<path d="m7.5 4.5 5 5.5-5 5.5"/>',
  back: '<path d="m12.5 4.5-5.5 5.5 5.5 5.5"/>',
  info: '<circle cx="10" cy="10" r="7.5"/><path d="M10 9v5M10 6.2v.1"/>',
  lib: '<path d="M5 2.5h6.5L15 6v11.5H5z"/><path d="M8 10h5M8 13h5"/>',
  chat: '<path d="M3.5 9.5c0-3.3 2.9-5.5 6.5-5.5s6.5 2.2 6.5 5.5S13.6 15 10 15c-.9 0-1.7-.1-2.5-.4L4 16l1-3c-.9-1-1.5-2.2-1.5-3.5Z"/>',
  clock: '<circle cx="10" cy="10" r="7"/><path d="M10 6v4.3l2.8 1.7"/>',
  gear: '<circle cx="10" cy="10" r="2.6"/><path d="M10 2.5v2M10 15.5v2M2.5 10h2M15.5 10h2M4.7 4.7l1.4 1.4M13.9 13.9l1.4 1.4M4.7 15.3l1.4-1.4M13.9 6.1l1.4-1.4"/>',
  send: '<path d="M3 10 16.5 3.5 13 17l-3-5.5z"/><path d="m10 11.5 6.5-8"/>',
  wifiOff: '<path d="M3 3l14 14M6.5 9.6a6 6 0 0 1 3-1.3M2.8 6.6A10 10 0 0 1 6 4.9M13.5 8.8c.7.3 1.3.8 1.8 1.3M10 15.5h.01M8 12.6a3 3 0 0 1 3.4-.4"/>',
  warn: '<path d="M10 3 18 17H2z"/><path d="M10 8v4M10 14.5v.1"/>',
  qr: '<path d="M3 7V3h4M13 3h4v4M17 13v4h-4M7 17H3v-4M6 10h8"/>',
  keys: '<rect x="2.5" y="5" width="15" height="10" rx="1.5"/><path d="M5.5 8h1M8.5 8h1M11.5 8h1M14.5 8h.01M6 12h8"/>',
  user: '<circle cx="10" cy="7" r="3.2"/><path d="M4 17c.8-3 3.2-4.5 6-4.5s5.2 1.5 6 4.5"/>',
  x: '<path d="m6 6 8 8M14 6l-8 8"/>',
  menu: '<path d="M3.5 5.5h13M3.5 10h13M3.5 14.5h9"/>',
  down: '<path d="m4.5 7.5 5.5 5 5.5-5"/>',
  sort: '<path d="M6 4v12M3.5 13.5 6 16l2.5-2.5M14 16V4M11.5 6.5 14 4l2.5 2.5"/>',
  panel: '<rect x="2.5" y="3.5" width="15" height="13" rx="2"/><path d="M12.5 3.5v13"/>',
  trash: '<path d="M3 5h14M7 5V2h6v3M5 5l1 13h8l1-13M8 8v7M12 8v7"/>',
};
function icon(name, size = 20) {
  const s = h('span', { 'aria-hidden': 'true' });
  s.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">${ICONS[name]}</svg>`;
  return s.firstChild;
}
function toast(msg) {
  document.querySelector('.toast')?.remove();
  const t = h('div', { class: 'toast', role: 'status' }, msg);
  app.append(t);
  setTimeout(() => t.remove(), 2600);
}


// ---------- article cache (IndexedDB, this page's own storage) ----------
// Recently opened items (original text loaded so far, translation pages, minutes, item meta) and
// the first library window, so reopening an item paints at once and stays readable offline.
// Private library text: IndexedDB only (never localStorage), one database per mode so demo data
// never mixes with the Mac's, bound to the current pairing (another pairing wipes it) and deleted
// on 重新配对. Entries are keyed [item_id, revision_id]; a new revision replaces the old entry.
// Only reads are cached: an edit's or request's outcome is never written here.
const CACHE_DB = GATEWAY ? 'memex.m.cache.gateway' : 'memex.m.cache.demo';
const CACHE_ITEMS = 30;
let cacheDb = null;
const idbDone = (req) => new Promise((res, rej) => { req.onsuccess = () => res(req.result); req.onerror = () => rej(req.error); });
const txDone = (t) => new Promise((res, rej) => { t.oncomplete = () => res(); t.onerror = t.onabort = () => rej(t.error); });
async function scopeHash(scope) {
  const d = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(String(scope)));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, '0')).join('');
}
async function cacheOpen(scope) {
  try {
    const req = indexedDB.open(CACHE_DB, 1);
    req.onupgradeneeded = () => {
      const items = req.result.createObjectStore('items', { keyPath: ['item_id', 'revision_id'] });
      items.createIndex('item', 'item_id');
      items.createIndex('used', 'used_at');
      req.result.createObjectStore('kv');
    };
    const db = await idbDone(req);
    const want = await scopeHash(scope);
    const have = await idbDone(db.transaction('kv').objectStore('kv').get('scope'));
    if (have !== want) {
      const t = db.transaction(['items', 'kv'], 'readwrite');
      t.objectStore('items').clear(); t.objectStore('kv').clear(); t.objectStore('kv').put(want, 'scope');
      await txDone(t);
    }
    cacheDb = db;
  } catch { cacheDb = null; } // no cache (private mode, quota): the app works online as before
}
async function cacheGet(itemId) {
  if (!cacheDb) return null;
  try {
    const all = await idbDone(cacheDb.transaction('items').objectStore('items').index('item').getAll(itemId));
    return all.sort((a, b) => b.used_at - a.used_at)[0] || null;
  } catch { return null; }
}
// Writes the item as currently loaded (same revision only) and evicts beyond CACHE_ITEMS (LRU).
async function cachePut(it) {
  // Not while a cached copy is unconfirmed, nor after a local edit (its outcome is not cached).
  if (!cacheDb || !it?.meta || it.cache || it.noCache || state.item !== it) return;
  const t = it.translation;
  const minutesRead = it.minutes?.read;
  const entry = {
    item_id: it.id, revision_id: it.meta.revision_id, used_at: Date.now(),
    meta: { ...it.meta, first_segment: null }, text: it.text, nextByte: it.nextByte, total: it.total,
    translation: t?.read ? { status: t.status, reason: t.reason ?? null, text: t.text || '', next: t.next ?? null,
      title: t.title ?? null, translatedTitle: t.translatedTitle ?? null, progress: t.progress ?? null } : null,
    minutes: minutesRead ? { state: it.minutes.state, reason: it.minutes.reason ?? null, text: it.minutes.text ?? null } : null,
    // Only turns read back from the Mac's stored conversation, never a local pending/failed one.
    chat: state.chats?.[it.id]?.hist === 'ok' ? state.chats[it.id].stored : null,
  };
  try {
    const tr = cacheDb.transaction('items', 'readwrite');
    const store = tr.objectStore('items');
    // One entry per item: older revisions go.
    for (const old of await idbDone(store.index('item').getAll(it.id))) if (old.revision_id !== entry.revision_id) store.delete([old.item_id, old.revision_id]);
    store.put(entry);
    await txDone(tr);
    it.cachedNext = entry.nextByte;
    const t2 = cacheDb.transaction('items', 'readwrite');
    const keys = await idbDone(t2.objectStore('items').index('used').getAllKeys());
    for (const k of keys.slice(0, Math.max(0, keys.length - CACHE_ITEMS))) t2.objectStore('items').delete(k);
    await txDone(t2);
  } catch { /* best effort */ }
}
async function cacheDrop(itemId) {
  if (!cacheDb) return;
  try {
    const t = cacheDb.transaction('items', 'readwrite');
    for (const k of await idbDone(t.objectStore('items').index('item').getAllKeys(itemId))) t.objectStore('items').delete(k);
    await txDone(t);
  } catch { /* best effort */ }
}
async function cacheCount() {
  try { return cacheDb ? await idbDone(cacheDb.transaction('items').objectStore('items').count()) : 0; } catch { return 0; }
}
async function cacheClear() {
  if (!cacheDb) return;
  try {
    const t = cacheDb.transaction(['items', 'kv'], 'readwrite');
    t.objectStore('items').clear(); t.objectStore('kv').delete('library');
    await txDone(t);
  } catch { /* best effort */ }
}
// Wipes everything including the pairing binding (重新配对).
async function cacheDestroy() {
  cacheDb?.close(); cacheDb = null;
  try { await idbDone(indexedDB.deleteDatabase(CACHE_DB)); } catch { /* best effort */ }
}
const cacheKv = async (key, value) => {
  if (!cacheDb) return null;
  try {
    if (value === undefined) return await idbDone(cacheDb.transaction('kv').objectStore('kv').get(key));
    const t = cacheDb.transaction('kv', 'readwrite'); t.objectStore('kv').put(value, key); await txDone(t);
  } catch { /* best effort */ }
  return null;
};
let cacheTimer = null;
function cacheSoon() { clearTimeout(cacheTimer); const it = state.item; cacheTimer = setTimeout(() => cachePut(it), 300); }

// ---------- theme: follow system by default, manual override in Settings ----------
const mq = matchMedia('(prefers-color-scheme: dark)');
function applyTheme() {
  const dark = state.theme === 'dark' || (state.theme === 'system' && mq.matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  document.querySelector('meta[name="theme-color"]:not([media])')?.remove();
  document.head.append(h('meta', { name: 'theme-color', content: dark ? '#1a1c20' : '#fbfcfd' }));
}
mq.addEventListener('change', applyTheme);
applyTheme();

// ---------- routing ----------
// Gateway mode routes in memory: the pairing protocol requires the page URL to carry no query or
// fragment. The demo preview keeps hash routes so review states can be linked.
let current = GATEWAY ? '/' : location.hash.replace(/^#/, '') || '/';
if (!GATEWAY) addEventListener('hashchange', () => {
  const next = location.hash.replace(/^#/, '') || '/';
  if (next !== current && route().name === 'item' && !editGuard()) { history.replaceState(null, '', `#${current}`); return; }
  current = next; render();
});
function route() {
  if (state.bootError) return { name: 'unsupported' };
  if (!state.paired) return { name: 'pair' };
  const m = current.match(/^\/item\/([\w-]+)$/);
  if (m) return { name: 'item', id: m[1] };
  return { name: { '/chat': 'chat', '/tasks': 'tasks', '/settings': 'settings', '/trash': 'trash' }[current] || 'library' };
}
function go(path) {
  // Leaving the reader with a chapter draft: the editor's navigation guard decides.
  if (path !== current && route().name === 'item' && !editGuard()) return;
  if (!GATEWAY && location.hash !== `#${path}`) { location.hash = path; return; } current = path; render(); }

function ribbon() {
  if (GATEWAY) return null;
  return h('div', { class: 'demo-ribbon' }, tx('演示版 · 合成示例数据，未连接你的 Mac，修改不会写入资料库'));
}

function tabbar(active) {
  const tabs = [['library', '/', 'lib', tx('资料库')], ['chat', '/chat', 'chat', tx('对话')], ['tasks', '/tasks', 'clock', tx('后台任务')], ['settings', '/settings', 'gear', tx('设置')]];
  return h('nav', { class: 'tabbar' }, tabs.map(([k, p, i, label]) =>
    h('button', { class: `tab${active === k ? ' on' : ''}`, onclick: () => go(p), 'aria-current': active === k ? 'page' : null }, icon(i, 22), label)));
}

// 正在重新连接 Mac… while the link is down (the relay closed it, e.g. 4002 mac_offline) or a read is
// waiting to be replayed: never 已连接 then.
const linkDown = () => GATEWAY && (state.reconnecting > 0 || !!conn?.reconnecting);
const RECONNECT_TEXT = () => tx('正在重新连接 Mac…');
function connChip() {
  const off = !state.online; const re = !off && linkDown();
  return h('button', { class: 'chip-conn', onclick: () => go('/settings'), 'data-link': off ? 'off' : re ? 'reconnecting' : 'on' },
    h('span', { class: `dot${off || re ? ' off' : ''}` }), off ? tx('未连接') : re ? RECONNECT_TEXT() : GATEWAY ? tx('已连接 · 我的 Mac') : tx('已连接 · 演示 Mac'), off ? icon('chev', 14) : null);
}
// A loading line that says 正在重新连接 Mac… while a read waits for the channel (repainted in place).
const waitText = (text) => (state.reconnecting > 0 ? RECONNECT_TEXT() : text);
const waitNote = (text, cls = 'note') => h('div', { class: cls, 'data-wait': text }, waitText(text));
function paintLink() {
  for (const el of document.querySelectorAll('.chip-conn')) el.replaceWith(connChip());
  for (const el of document.querySelectorAll('[data-wait]')) el.textContent = waitText(el.dataset.wait);
  paintLibraryStatus();
}

// ---------- pairing (m2-1) ----------
// The real pairing code is a long one-time string (QR or paste), validated by the existing
// /remote/v1 protocol module inside this page; it never travels in a URL.
function renderPair() {
  const ta = h('textarea', { class: 'pair-code', rows: '3', placeholder: tx('粘贴 Mac 上显示的配对码'), autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', 'aria-label': tx('配对码') });
  const status = h('div', { class: 'small', role: 'status' }, tx('配对码 5 分钟内有效，只在本页使用。'));
  const video = h('video', { class: 'scan-video', playsinline: true, muted: true, hidden: true });
  const canvas = h('canvas', { hidden: true });
  let scanCtl = null;
  const connect = async (code) => {
    if (!GATEWAY) { toast(tx('演示版不会真正配对；正式版在这里连接你的 Mac')); return; }
    if (!code.trim()) { ta.focus(); return; }
    status.textContent = tx('正在连接 Mac…');
    try { await conn.pair(code); await cacheOpen(conn.scope); state.paired = true; ta.value = ''; go('/'); } catch (e) {
      status.textContent = e?.code === 'unavailable' ? tx('无法连接到 Mac。请确认 Mac 在线、配对码未过期后重试。') : tx('配对码无效或已过期，请在 Mac 上重新生成。');
    }
  };
  const scan = async () => {
    if (!GATEWAY) { toast(tx('演示版不打开相机；正式版在这里扫描 Mac 上的二维码')); return; }
    if (scanCtl) { scanCtl.abort(); return; }
    scanCtl = new AbortController(); video.hidden = false; status.textContent = tx('请将 Mac 上的二维码放入取景框。');
    try { await connect(await conn.scan({ video, canvas, signal: scanCtl.signal })); } catch (e) {
      if (e?.message !== 'scan_cancelled') status.textContent = tx('无法使用相机扫描，可改为粘贴配对码。');
    } finally { scanCtl = null; video.hidden = true; }
  };
  return h('div', { class: 'screen' }, ribbon(),
    h('div', { class: 'pair' },
      h('img', { class: 'logo', src: 'assets/icon-192.png', alt: '' }),
      h('div', { class: 'brand serif' }, BRAND_NAME),
      h('div', { class: 'tagline' }, tx('你的资料库，留在你的 Mac 上')),
      h('hr'),
      h('h2', { class: 'serif' }, tx('连接到你的 Mac')),
      h('p', {}, tx('在 Mac 上的 {0} 打开“远程访问”，扫描二维码或粘贴配对码。数据始终留在你的 Mac 上。', BRAND_NAME)),
      h('button', { class: 'btn-wide primary', onclick: scan }, icon('qr'), tx('扫描配对二维码')),
      video, canvas,
      ta,
      h('button', { class: 'btn-wide', onclick: () => connect(ta.value) }, icon('keys'), tx('用配对码连接')),
      status,
      GATEWAY ? null : h('button', { class: 'linkish', onclick: () => { state.paired = true; localStorage.setItem('memex.m.paired', '1'); go('/'); } }, tx('先看演示资料库 →')),
    ));
}

function renderUnsupported() {
  return h('div', { class: 'screen' }, h('div', { class: 'pair' },
    h('img', { class: 'logo', src: 'assets/icon-192.png', alt: '' }),
    h('h2', { class: 'serif' }, tx('无法建立安全连接')),
    h('p', {}, state.bootError === 'another_tab_active' ? tx('{0} 已在另一个标签页打开，请关闭其他标签页后刷新。', BRAND_NAME) : tx('这个浏览器不支持所需的加密能力（需要 iOS 18.4 或更新的 Safari）。'))));
}

// Hosts only for web sources; local imports (urn:, file:) never show their raw identifier.
const showSource = (v) => (!v ? tx('来源未知') : /^https?:\/\//.test(v) ? v.replace(/^https?:\/\//, '') : /^[a-z][a-z0-9+.-]*:/i.test(v) ? tx('本地文件') : v);
const showDate = (v, n) => (v ? v.replace('T', ' ').slice(0, n) : tx('日期未知'));

// Fourth-round UI approved on 2026-11-03: version/appearance, dial/stats and sidebar/sort.
// Published for every phone; the old per-device draft preference no longer gates these controls.
const DRAFTS = true;
// App version: the newest entry of changelog.mjs, the single source shared with the desktop
// (its workspace version must match; a desktop test checks it). v0.1.x = the x-th release.
// The build itself is identified by the gateway's hash of the served bundle (session.info).
// Must equal the newest entry in changelog.mjs (the Mac's changelog test checks this line).
const APP_VERSION = '0.1.48';
const BUILD_DATE = '2026-11-12';

// Portrait and avatar: the gateway serves every file with no-store, so each image is kept in the
// window's Cache Storage (one cache per app version, older ones deleted) and drawn from one blob:
// URL per asset for the session. No service worker. Any failure falls back to the plain src.
// The gateway's connect-src is the relay's ws: origin, so static files cannot be fetch()ed: on
// first use the image loads once through img-src and its pixels are stored as PNG.
const ASSET_CACHE = `memex-assets-${APP_VERSION}`;
const assetUrls = new Map(); // src → blob: URL (or src on fallback) | pending Promise
let assetPrune = null;
async function loadAsset(src) {
  if (!globalThis.caches) return src;
  assetPrune ??= caches.keys().then((ks) => Promise.all(ks.filter((k) => k.startsWith('memex-assets-') && k !== ASSET_CACHE).map((k) => caches.delete(k)))).catch(() => {});
  const url = new URL(src, location.href).href;
  const cache = await caches.open(ASSET_CACHE);
  let blob = await (await cache.match(url))?.blob();
  if (!blob?.size || !blob.type.startsWith('image/')) {
    const img = new Image();
    img.src = url;
    await img.decode();
    const canvas = document.createElement('canvas');
    canvas.width = img.naturalWidth; canvas.height = img.naturalHeight;
    canvas.getContext('2d').drawImage(img, 0, 0);
    blob = await new Promise((ok, no) => canvas.toBlob((b) => (b?.size ? ok(b) : no(new Error('asset'))), 'image/png'));
    await cache.put(url, new Response(blob, { headers: { 'content-type': 'image/png' } }));
  }
  return URL.createObjectURL(blob);
}
function assetImg(attrs) {
  const { src } = attrs;
  const img = h('img', { ...attrs, src: null });
  const plain = () => { img.classList.remove('asset-wait'); if (img.getAttribute('src') !== src) img.src = src; };
  img.addEventListener('error', () => { if (img.src.startsWith('blob:')) { assetUrls.set(src, src); plain(); } });
  const set = (u) => { img.classList.remove('asset-wait'); img.src = u; };
  let u = assetUrls.get(src);
  if (typeof u === 'string') { set(u); return img; }
  if (!u) {
    u = loadAsset(src).catch(() => src).then((v) => { assetUrls.set(src, v); return v; });
    assetUrls.set(src, u);
  }
  img.classList.add('asset-wait');
  u.then(set, plain);
  return img;
}
// Text and font assets (简繁 dictionaries, 寒蝉拙楷): fetched only on first use and kept in the same
// versioned Cache Storage (the gateway serves no-store). A page whose connect-src refuses fetch()
// rejects here; the callers say so (dictionaries) or fall back to the font's url() (font-src).
async function assetBytes(src) {
  const url = new URL(src, location.href).href;
  const cache = globalThis.caches ? await caches.open(ASSET_CACHE).catch(() => null) : null;
  if (cache) assetPrune ??= caches.keys().then((ks) => Promise.all(ks.filter((k) => k.startsWith('memex-assets-') && k !== ASSET_CACHE).map((k) => caches.delete(k)))).catch(() => {});
  let res = await cache?.match(url).catch(() => null);
  if (!res) {
    res = await fetch(url, { credentials: 'same-origin' });
    if (!res.ok) throw new Error('asset');
    await cache?.put(url, res.clone()).catch(() => {});
  }
  return res.arrayBuffer();
}

// ---------- 古籍阅读: 简繁 (display only), 竖排, 寒蝉拙楷; remembered per item ----------
// { zh: 'orig' | 's' (简体) | 't' (繁体), vertical, font: null (follow the reading font; 竖排 → 寒蝉拙楷) | 'sans' | 'serif' | 'kai' }
const gujiMem = new Map();
function gj(id) {
  if (!gujiMem.has(id)) {
    let g = {};
    try { g = JSON.parse(localStorage.getItem(`memex.m.guji|${id}`) || '{}') || {}; } catch { g = {}; }
    gujiMem.set(id, { zh: ['s', 't'].includes(g.zh) ? g.zh : 'orig', vertical: g.vertical === true, font: ['sans', 'serif', 'kai'].includes(g.font) ? g.font : null });
  }
  return gujiMem.get(id);
}
function gjSet(id, patch) {
  const g = { ...gj(id), ...patch }; gujiMem.set(id, g);
  if (g.zh === 'orig' && !g.vertical && !g.font) localStorage.removeItem(`memex.m.guji|${id}`);
  else localStorage.setItem(`memex.m.guji|${id}`, JSON.stringify(g));
}
const readFont = (it) => { const g = gj(it.id); return g.font || (vertNow(it) ? 'kai' : state.readingFont); };
// The article's font class; selecting 寒蝉拙楷 is what starts its (one-time) download.
function fontCls(it) {
  const f = it ? readFont(it) : state.readingFont;
  if (f === 'kai') loadKai();
  return f === 'serif' ? ' serif' : f === 'kai' ? ' kai' : '';
}
// 竖排 applies to one text on screen (原文 or 译文); 对照 always reads 横排.
const vertNow = (it) => !!it && gj(it.id).vertical && state.lang !== 'bi' && state.insView === 'text';
// 简繁 and 竖排 are offered on Chinese text: a book with a Chinese edition, or an item that reads as Chinese.
const zhItem = (it) => (it.book ? [it.book.o?.language, it.book.t?.language].some((l) => /^zh/i.test(l || '')) : !!it.text && looksChinese(it));

// 寒蝉拙楷 (SIL OFL 1.1): one FontFace from the cached bytes, font-display swap (the 楷体 fallback shows
// until it is ready); never fetched unless a reader selects it.
const KAI_SRC = 'assets/fonts/ChillZhuoKai.woff2';
let kaiState = null; // null | 'loading' | 'ready' | 'failed'
function loadKai() {
  if (kaiState || !document.fonts || typeof FontFace !== 'function') return;
  kaiState = 'loading';
  const add = (src) => new FontFace('ChillZhuoKai', src, { display: 'swap' }).load().then((f) => { document.fonts.add(f); });
  assetBytes(KAI_SRC).then((buf) => add(buf), () => add(`url(${new URL(KAI_SRC, location.href).href}) format('woff2')`))
    .then(() => { kaiState = 'ready'; }, () => { kaiState = 'failed'; });
}

// 简繁 converters (OpenCC dictionaries, see zhconv.mjs), built on the first switch; conversions of
// repeated text (each render redraws the chapter) are memoised per direction.
const zhConv = { s: null, t: null }; const zhPending = {};
function zhLoad(dir) {
  if (zhConv[dir]) return Promise.resolve(zhConv[dir]);
  zhPending[dir] ??= (async () => {
    const { makeConverter, ZH_DICTS } = await import('./zhconv.mjs');
    const dec = new TextDecoder();
    const [phrases, chars] = await Promise.all(ZH_DICTS[dir].map(async (f) => dec.decode(await assetBytes(f))));
    const conv = makeConverter(phrases, chars); const memo = new Map();
    zhConv[dir] = (text) => {
      let v = memo.get(text);
      if (v === undefined) { if (memo.size > 20000) memo.clear(); v = conv(text); memo.set(text, v); }
      return v;
    };
    return zhConv[dir];
  })().catch((e) => { delete zhPending[dir]; throw e; });
  return zhPending[dir];
}
// The open item's 简繁 display conversion (null: 原文, or the dictionary is not loaded yet).
function curZh() {
  const it = state.item; const z = it ? gj(it.id).zh : 'orig';
  if (z === 'orig') return null;
  if (!zhConv[z]) zhLoad(z).then(() => { if (state.item === it) render(); }, () => {});
  return zhConv[z];
}
const zhShow = (text) => { const f = curZh(); return f && text ? f(text) : text; };

// Loaded when 更新日志 opens, so the rest of the app never depends on it.
let CHANGELOG = null;

// Item types as the desktop labels them (memex-client dto.rs item_type_label); unknown values as sent.
const TYPE_LABELS = { webpage: '网页', web_page: '网页', web_article: '网页', journal_article: '期刊论文', article: '期刊论文', book: '书籍',
  book_section: '书籍章节', conference_paper: '会议论文', thesis: '学位论文', report: '报告', preprint: '预印本', magazine_article: '杂志文章',
  newspaper_article: '报纸文章', blog_post: '博客文章', document: '文档', manuscript: '手稿', presentation: '演示文稿', video: '视频',
  film: '视频', video_recording: '视频', podcast: '播客', pdf: 'PDF', note: '笔记', evernote_note: '笔记', attachment: '附件', letter: '信件',
  interview: '访谈', email: '电子邮件', forum_post: '论坛帖子', encyclopedia_article: '百科条目', dictionary_entry: '词典条目', patent: '专利',
  audio_recording: '音频', artwork: '艺术作品', computer_program: '软件', dataset: '数据集', standard: '标准', map: '地图', statute: '法规',
  case: '案例', hearing: '听证', instant_message: '即时消息', tv_broadcast: '广播节目', radio_broadcast: '广播节目',
  social_media_post: '社交媒体帖子', tweet: '社交媒体帖子', other: '其他类型' };
function typeLabel(t) {
  if (!t) return tx('类型未知');
  const k = t.trim().replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`).replace(/[- ]/g, '_');
  return TYPE_LABELS[k] ? tx(TYPE_LABELS[k]) : t;
}

// 来源 grouped by registrable domain under a short site name, exactly as the desktop sidebar
// (memex-library sources.rs): sub-sites fold into their site, one-item sources into 其他来源.
// Non-http sources are never shown raw: they all read 本地文件 (one entry).
const TWO_LABEL_SUFFIXES = new Set(['com.cn', 'net.cn', 'org.cn', 'gov.cn', 'edu.cn', 'ac.cn', 'com.hk', 'org.hk', 'com.tw', 'org.tw', 'co.uk', 'ac.uk',
  'org.uk', 'co.jp', 'ne.jp', 'or.jp', 'com.au', 'co.kr', 'com.sg', 'co.nz', 'com.br']);
const SITE_NAMES = { 'mp.weixin.qq.com': '微信公众号', '36kr.com': '36氪', 'sspai.com': '少数派', 'huxiu.com': '虎嗅', 'zhihu.com': '知乎',
  'thepaper.cn': '澎湃新闻', 'caixin.com': '财新', 'jiqizhixin.com': '机器之心', 'qbitai.com': '量子位', 'geekpark.net': '极客公园',
  'ifanr.com': '爱范儿', 'latepost.com': '晚点 LatePost', 'infoq.cn': 'InfoQ', 'ithome.com': 'IT之家', 'jiemian.com': '界面新闻',
  'yicai.com': '第一财经', 'cls.cn': '财联社', 'guancha.cn': '观察者网', 'douban.com': '豆瓣', 'weibo.com': '微博', 'bilibili.com': '哔哩哔哩',
  'xiaohongshu.com': '小红书', '163.com': '网易', 'sina.com.cn': '新浪', 'qq.com': '腾讯', 'sohu.com': '搜狐', 'youtube.com': 'YouTube',
  'youtu.be': 'YouTube', 'x.com': 'X', 'twitter.com': 'X', 'github.com': 'GitHub', 'arxiv.org': 'arXiv', 'medium.com': 'Medium',
  'substack.com': 'Substack', 'nytimes.com': '纽约时报', 'ft.com': '金融时报', 'wsj.com': '华尔街日报', 'bloomberg.com': '彭博',
  'reuters.com': '路透', 'economist.com': '经济学人', 'wikipedia.org': '维基百科' };
function registrableDomain(host) {
  host = (host || '').split('/')[0].split(':')[0].replace(/\.$/, '').toLowerCase();
  if (!host.includes('.') || /^[\d.]+$/.test(host) || !/^[a-z0-9.-]+$/.test(host)) return null;
  const own = Object.keys(SITE_NAMES).find((d) => d.split('.').length >= 3 && (host === d || host.endsWith(`.${d}`)));
  if (own) return own;
  const labels = host.split('.');
  return labels.slice(-(labels.length >= 3 && TWO_LABEL_SUFFIXES.has(labels.slice(-2).join('.')) ? 3 : 2)).join('.');
}
const siteName = (domain) => SITE_NAMES[domain] || domain;
const LOCAL = 'local';
// {key, name, sub} for one source value; sub names the exact origin inside its group.
function sourceKey(v) {
  if (!v) return { key: '', name: tx('来源未知'), sub: tx('来源未知') };
  if (/^https?:\/\//i.test(v)) {
    const host = v.replace(/^https?:\/\//i, '').split('/')[0];
    const d = registrableDomain(host);
    return d ? { key: d, name: siteName(d), sub: host } : { key: host, name: host, sub: host };
  }
  const zotero = /^urn:zotero:group:(\d+)$/.exec(v);
  return { key: LOCAL, name: tx('本地文件'), sub: zotero ? tx('Zotero 群组 {0}', zotero[1]) : /^urn:sha256:/i.test(v) ? tx('本地导入（按内容识别）') : /^file:/i.test(v) ? tx('本机文件路径') : tx('其他本地来源') };
}
const sourceLabel = (v) => { const k = sourceKey(v); return k.key === LOCAL || k.name === k.sub ? k.name : `${k.name} · ${k.sub}`; };
function groupSources(sources) {
  const groups = new Map();
  for (const f of sources) {
    const k = sourceKey(f.value);
    const g = groups.get(k.key) || { key: k.key, name: k.name, count: 0, members: [] };
    g.count += f.count; g.members.push({ ...f, sub: k.sub }); groups.set(k.key, g);
  }
  const keep = []; const others = [];
  for (const g of groups.values()) (g.count > 1 ? keep : others).push(...(g.count > 1 ? [g] : g.members));
  keep.forEach((g) => g.members.sort((a, b) => b.count - a.count || (a.value < b.value ? -1 : 1)));
  keep.sort((a, b) => b.count - a.count || (a.name < b.name ? -1 : 1));
  others.sort((a, b) => (a.sub < b.sub ? -1 : 1));
  return { groups: keep, others, othersCount: others.reduce((n, f) => n + f.count, 0) };
}

// Library sort keys (the query's `sort`, all newest/A→Z as noted). Exactly the desktop's set.
const SORTS = [['captured_at', '抓取时间', true], ['added_at', '最近加入资料库', true], ['published_at', '原始发表日期', true], ['title', '标题', false], ['source', '来源', false]];
// added_at is the 2026-11-03 candidate: tried, and marked unsupported if this Mac rejects it.
// published_at only when the Mac advertises it in query_sorts together with
// query_date_policy "publication_or_capture_v1" (memex.library_query.md); a bare legacy listing is not enough.
const sortServed = (k) => !GATEWAY || (k === 'added_at' ? !state.addedAtRejected
  : (state.caps?.query_sorts || ['title', 'source', 'captured_at', 'item_type']).includes(k) && (k !== 'published_at' || state.caps?.query_date_policy === 'publication_or_capture_v1'));
// Duration (duration_seconds) as h:mm:ss, or mm:ss under an hour; unknown → null.
function fmtDuration(s) {
  if (!Number.isInteger(s) || s <= 0) return null;
  const p2 = (n) => String(n).padStart(2, '0');
  const hh = Math.floor(s / 3600); const mm = Math.floor((s % 3600) / 60); const ss = s % 60;
  return hh ? `${hh}:${p2(mm)}:${p2(ss)}` : `${p2(mm)}:${p2(ss)}`;
}
// List row / reader: the publication day (published_at) and the duration when the item has one.
const pubParts = (it) => [it.published_at && tx('发布 {0}', it.published_at), fmtDuration(it.duration_seconds)].filter(Boolean);
const originServed = () => !GATEWAY || state.caps?.query_origin === 'available';
function rowDate(it) {
  const s = state.lib.query.sort;
  // Unknown added / published dates show nothing (never a stand-in date).
  if (s === 'added_at') return it.added_at ? tx('加入 {0}', it.added_at.slice(5, 10)) : null;
  if (s === 'published_at') return null; // the publication day itself follows (pubParts)
  return it.captured_at ? it.captured_at.slice(5, 10) : tx('日期未知');
}
// Small label + a transparent native picker over it (the iOS wheel; 16px so focus never zooms).
function sortControl() {
  if (!DRAFTS) return null;
  const q = state.lib.query;
  const cur = SORTS.find(([k]) => k === q.sort) || SORTS[0];
  const sel = h('select', { class: 'sort-pick', 'aria-label': tx('排序方式') },
    SORTS.map(([k, label]) => h('option', { value: k, selected: k === q.sort, disabled: !sortServed(k) }, sortServed(k) ? tx(label) : tx('{0}（这台 Mac 暂不支持）', tx(label)))));
  sel.addEventListener('change', () => {
    const k = sel.value; const def = SORTS.find(([x]) => x === k);
    Object.assign(q, { sort: k, descending: def[2] }); localStorage.setItem('memex.m.sort', k);
    state.lib.scroll = 0; loadLibrary(true);
  });
  return h('span', { class: 'sort-ctl' }, icon('sort', 14), h('span', {}, tx(cur[1])), sel);
}
// Origin filter: 我抓取的 / 导入的 / 全部 (needs query.origin from the Mac; else not offered).
function originControl() {
  if (!DRAFTS || !originServed()) return null;
  const q = state.lib.query;
  return h('div', { class: 'origin-seg', role: 'tablist', 'aria-label': tx('来源类别') },
    [['capture', tx('我抓取的')], [null, tx('全部')], ['import', tx('导入的')]].map(([k, label]) => h('button', {
      class: q.origin === k ? 'on' : '', role: 'tab', 'aria-selected': String(q.origin === k),
      onclick: () => { q.origin = k; localStorage.setItem('memex.m.origin', k ?? 'all'); state.lib.scroll = 0; render(); loadLibrary(true); },
    }, label)));
}

// ---------- library (m1-1, m2-2 offline) ----------
// Paged library_query: first window at offset 0 with view=null, later windows carry the view;
// stale_query_view restarts from the first window instead of mixing pages.
// library_loading after the bounded retries (gateway-source.mjs): the Mac's query slot is busy. Never a
// sign of bad data: caches, remembered AI versions, drafts and request ids are all kept. Same wording
// as the Mac client.
const MAC_BUSY = 'library_loading';
const BUSY_TEXT = tx('Mac 正忙，稍后重试');
const ERR_TEXT = { library_loading: BUSY_TEXT, unavailable: tx('无法连接到你的 Mac'), stale_item_revision: tx('条目已在 Mac 上更新，请返回后重新打开'), context_resource_not_found: tx('条目不存在或无权访问') };
function noteOutcome(err) {
  const wasOnline = state.online;
  state.online = !(err && err.code === 'unavailable');
  return wasOnline !== state.online;
}
async function loadLibrary(reset) {
  const L = state.lib;
  if (L.loading) { if (reset) L.reloadPending = true; return; }
  if (!reset && L.next == null) return;
  L.reloadPending = false;
  L.loading = true; L.error = null;
  if (reset) { L.view = null; L.next = 0; }
  // The window is fixed now: the cached first window may be painted (next: null) while the
  // capabilities are read or the link comes back.
  const offset = L.next; const view = L.view;
  paintLibraryStatus();
  let replaced = reset;
  try {
    // Discover filters before the first query, not only after opening an item or Settings.
    if (GATEWAY && !state.caps) { await caps(); render(); }
    // Only what this Mac serves is sent: an unserved sort falls back to 抓取时间, origin is dropped.
    const query = { ...L.query, sort: sortServed(L.query.sort) ? L.query.sort : 'captured_at', origin: originServed() ? L.query.origin : null };
    const r = await source.libraryQuery({ query, offset, limit: PAGE, expected_view: view });
    if (reset || L.view !== r.view) { L.rows = []; replaced = true; }
    Object.assign(L, { view: r.view, total: r.total, next: r.next_offset, sources: r.sources, item_types: r.item_types });
    L.rows.push(...r.items);
    L.cached = false;
    noteOutcome(null);
    const q = L.query;
    if (r.offset === 0 && !q.text && !q.source && !q.item_type && !q.group_id && query.sort === 'captured_at' && q.descending && !query.origin) {
      cacheKv('library', { items: r.items, total: r.total, sources: r.sources, item_types: r.item_types });
    }
  } catch (e) {
    L.loading = false;
    if (e.code === 'stale_query_view') return loadLibrary(true);
    if (e.code === 'invalid_request' && L.query.sort === 'added_at' && GATEWAY && !state.addedAtRejected) {
      state.addedAtRejected = true; toast(tx('这台 Mac 还不支持按加入时间排序，已改回抓取时间'));
      Object.assign(L.query, { sort: 'captured_at', descending: true }); localStorage.setItem('memex.m.sort', 'captured_at'); render();
      return loadLibrary(true);
    }
    L.error = e.code || 'unknown';
    noteOutcome(e);
    replaced = false;
  }
  L.loading = false;
  if (L.reloadPending) return loadLibrary(true);
  refreshLibrary(replaced);
}
function paintLibraryStatus() {
  const el = document.getElementById('count-line');
  if (el && state.lib.loading) el.textContent = state.reconnecting > 0 ? (state.lib.rows.length ? `${tx('{0} 条', state.lib.total)} · ${RECONNECT_TEXT()}` : RECONNECT_TEXT())
    : state.lib.rows.length ? tx('{0} 条 · 载入中…', state.lib.total) : tx('载入中…');
}

// The library screen is built once and then patched, so the search field keeps focus (and the
// phone keyboard stays up) and the list keeps its scroll position while windows arrive.
let lib = null; // { screen, list, rows, banner, facets, chip, rendered }
let libObserver;
// Titles (library_query candidate 2026-11-03): with an optional translated_title, the translation
// leads, then a visible separator, then the source title; without it, the title alone.
function titleParts(title, translated) {
  const t = typeof translated === 'string' ? translated.trim() : '';
  return t && t !== title ? [h('span', { class: 'tt-main' }, t), h('span', { class: 'tt-sep', 'aria-hidden': 'true' }, ' ｜ '), h('span', { class: 'tt-orig' }, title)] : [title];
}
function libraryRow(it) {
  const root = currentLibrary();
  const open = h('button', { class: 'row', onclick: () => { state.lib.scroll = lib.list.scrollTop; go(`/item/${it.item_id}`); } },
    h('span', { class: 'doc' }, icon('doc', 22)),
    h('span', { class: 'main' },
      h('div', { class: 't serif' }, ...titleParts(it.title, it.translated_title)),
      // 发布 date / duration never break inside (CJK and the date's hyphens would wrap anywhere).
      h('div', { class: 'm' }, [showSource(it.source), typeLabel(it.item_type), rowDate(it), ...pubParts(it).map((t) => h('span', { class: 'nw' }, t))]
        .filter(Boolean).flatMap((x, i) => (i ? [' · ', x] : [x])))),
    h('span', { class: 'chev' }, icon('chev', 16)));
  return root && trashEditable() ? h('div', { class: 'library-item' }, open,
    h('button', { class: 'icon-btn trash-delete', 'aria-label': tx('移入本库垃圾桶'), onclick: () => deleteFromLibrary(root, it) }, icon('trash', 18))) : open;
}

// Library-scoped deletion keeps the item in other libraries and preserves its original folders.
const trashServed = () => state.caps?.group_trash === 'available';
const trashEditable = () => trashServed() && state.caps?.scopes?.includes('edit');
function currentLibrary() {
  const nodes = state.groups?.nodes || [];
  const selected = nodes.find((n) => n.id === state.lib.query.group_id);
  return selected ? nodes.find((n) => n.id === selected.root_group_id) : null;
}
async function openTrash(root) {
  reportActiveGroup(null);
  state.trash = { root, items: [], next: null, loading: false, error: null };
  state.side = null; go('/trash');
  await loadTrash(true);
}
async function loadTrash(reset) {
  const t = state.trash;
  if (!t || t.loading) return;
  t.loading = true; t.error = null; render();
  try {
    const page = await source.groupTrash(t.root.id, reset ? null : t.next);
    if (!Array.isArray(page.items)) throw new Error('invalid_response');
    t.items = reset ? page.items : [...t.items, ...page.items]; t.next = page.next_cursor;
    noteOutcome(null);
  } catch (e) { t.error = e.code || 'unknown'; noteOutcome(e); }
  t.loading = false;
  if (state.trash === t && route().name === 'trash') render();
}
let trashWrite = false;
async function deleteFromLibrary(root, item) {
  if (trashWrite || !trashEditable()) return;
  trashWrite = true;
  try {
    await source.editGroupTrash({ group_id: root.id, item_id: item.item_id, expected_revision_id: item.revision_id, request_id: crypto.randomUUID(), restore_plan: null });
    toast(tx('已移入本库垃圾桶；其他库不受影响'));
  } catch (e) { toast(tx('删除未确认，请查看本库垃圾桶：{0}', reasonText(e.code || 'unknown'))); }
  finally { trashWrite = false; }
  await loadGroups();
  // A write timeout is not retried. Read the live list and the trash to resolve its outcome.
  await loadLibrary(true);
}
async function restoreFromTrash(item) {
  const t = state.trash;
  if (!t || trashWrite || !trashEditable()) return;
  trashWrite = true; render();
  try {
    await source.editGroupTrash({ group_id: t.root.id, item_id: item.item_id, expected_revision_id: item.revision_id, request_id: crypto.randomUUID(), restore_plan: item.plan_id });
    toast(tx('已恢复到本库原目录'));
  } catch (e) { toast(tx('恢复未确认，请刷新垃圾桶')); }
  finally { trashWrite = false; }
  if (state.trash === t) await loadTrash(true);
  await loadGroups(); await loadLibrary(true);
}
function renderTrash() {
  const t = state.trash;
  return h('div', { class: 'screen' }, ribbon(),
    h('header', { class: 'top topbar' }, h('button', { class: 'icon-btn', 'aria-label': tx('打开资料库导航'), onclick: () => openSide('left') }, icon('menu', 22)),
      h('div', { class: 'grow' }, t ? tx('{0} · 垃圾桶', t.root.name) : tx('垃圾桶')),
      h('button', { class: 'btn-sm', disabled: t?.loading, onclick: () => loadTrash(true) }, tx('刷新'))),
    h('div', { class: 'scroll trash-list' }, h('p', { class: 'note' }, tx('恢复后回到本库原目录，其他库不受影响。')),
      t?.error ? h('div', { class: 'banner warn' }, tx('垃圾桶读取失败，请刷新')) : null,
      ...(t?.items || []).map((item) => h('div', { class: 'trash-row' },
        h('div', { class: 'grow' }, h('div', { class: 'serif' }, item.title), h('div', { class: 'small' }, showDate(item.trashed_at, 16))),
        h('button', { class: 'btn-sm', disabled: trashWrite || !trashEditable(), onclick: () => restoreFromTrash(item) }, tx('恢复')))),
      t?.loading ? h('div', { class: 'empty' }, tx('载入中…')) : !t?.items.length && !t?.error ? h('div', { class: 'empty' }, tx('垃圾桶是空的')) : null,
      t?.next && !t.loading ? h('button', { class: 'btn', onclick: () => loadTrash(false) }, tx('继续载入')) : null), tabbar('library'));
}
// 当前分组 (ui_active_group_v1): an explicit tap on a library root or a folder of any depth tells the
// Mac which group the Chrome extension files into when 收进 Inbox is off; a tap on a virtual view
// (全部 / 类型 / 来源 / 垃圾桶 / 清除筛选) sends null. Called only from those taps, never on restore, cache,
// refresh, offline recovery or reconnect. Silent: never shown, never waited for by navigation. Each tap
// gets a new request_id; one request is in flight at a time and a newer tap replaces the pending one
// (an older tap is never re-queued). An `unknown` reply is resent once with the SAME request_id, then
// dropped. Only when served and the pairing has the edit grant.
let activeGroupNext = null; let activeGroupBusy = false;
function reportActiveGroup(group_id) {
  activeGroupNext = { request_id: crypto.randomUUID(), group_id, sent: 0 };
  if (!activeGroupBusy) pumpActiveGroup();
}
async function pumpActiveGroup() {
  activeGroupBusy = true;
  try {
    while (activeGroupNext) {
      const job = activeGroupNext;
      let served = false;
      try { await caps(); served = state.caps?.ui_active_group_v1 === 'available' && !!state.caps?.scopes?.includes('edit'); } catch { /* no session.info: not sent */ }
      let again = false;
      if (served && activeGroupNext === job) {
        job.sent += 1;
        try { await source.uiActiveGroupSet({ request_id: job.request_id, group_id: job.group_id }); } catch (e) { again = e?.code === 'unknown' && job.sent < 2; }
      }
      if (activeGroupNext === job && !again) activeGroupNext = null;
    }
  } finally { activeGroupBusy = false; }
}
// Type / source filters live in the left sidebar; the list shows the active one as a removable chip.
function pickFacet(source_, type_, group = null) {
  reportActiveGroup(group?.id ?? null);
  const L = state.lib;
  Object.assign(L.query, { source: source_, item_type: type_, group_id: group?.id ?? null, groupName: group?.name ?? null, include_descendants: true });
  if (group) {
    L.query.origin = null;
    localStorage.setItem('memex.m.origin', 'all');
  }
  L.scroll = 0; state.side = null;
  if (route().name !== 'library') { go('/'); } else render();
  loadLibrary(true);
}
function libraryFacets() {
  const q = state.lib.query;
  const sort = sortControl();
  if (!q.source && !q.item_type && !q.group_id) return h('span', { class: 'count-tools' }, sort);
  return h('span', { class: 'count-tools' }, h('button', { class: 'filter-chip', 'aria-label': tx('清除筛选'), onclick: () => pickFacet(null, null) },
    h('span', { class: 'fc-t' }, q.group_id ? tx('分组：{0}', q.groupName) : q.item_type ? tx('类型：{0}', typeLabel(q.item_type)) : tx('来源：{0}', sourceLabel(q.source))), icon('x', 14)), sort);
}
function libraryBanner() {
  const L = state.lib;
  return !state.online ? h('div', { class: 'banner' },
    h('span', { class: 'rose-ic' }, icon('wifiOff')),
    h('span', { class: 'grow' }, L.rows.length ? tx('无法连接到你的 Mac，显示的是上次载入的列表。') : tx('无法连接到你的 Mac。')),
    h('button', { class: 'btn-sm', onclick: () => loadLibrary(L.rows.length === 0 || L.view == null) }, tx('重试')))
    : L.error ? h('div', { class: 'banner warn', 'data-busy': L.error === MAC_BUSY ? '1' : null, 'data-error': L.error }, h('span', { class: 'grow' }, L.error === MAC_BUSY ? BUSY_TEXT : tx('资料库载入失败。')), h('button', { class: 'btn-sm', onclick: () => loadLibrary(true) }, tx('重试'))) : null;
}
function refreshLibrary(replaced) {
  if (!lib || !lib.screen.isConnected) return;
  const L = state.lib;
  lib.banner.replaceChildren(...[libraryBanner()].filter(Boolean));
  const chip = connChip(); lib.chip.replaceWith(chip); lib.chip = chip;
  if (replaced) {
    const facets = libraryFacets(); lib.facets.replaceWith(facets); lib.facets = facets;
    const fr = libraryFormatRow(); lib.fmtRow.replaceWith(fr); lib.fmtRow = fr;
    lib.rows.replaceChildren(); lib.rendered = 0; lib.list.scrollTop = 0;
  }
  const frag = document.createDocumentFragment();
  for (let i = lib.rendered; i < L.rows.length; i += 1) frag.append(libraryRow(L.rows[i]));
  lib.rows.append(frag); lib.rendered = L.rows.length;
  if (!L.rows.length && !L.loading) lib.rows.replaceChildren(...(!L.error && state.online ? [h('div', { class: 'empty' }, tx('没有匹配的条目'))] : []));
  lib.countText.textContent = L.loading && !L.rows.length ? waitText(tx('载入中…')) : !L.rows.length && (L.error || !state.online) ? ''
    : L.cached ? tx('{0} 条 · 缓存版本', L.total) : tx('{0} 条', L.total);
}

function renderLibrary() {
  const L = state.lib;
  const list = h('div', { class: 'scroll', id: 'lib-scroll' });
  const rows = h('div');
  const countText = h('span', { id: 'count-line' });
  const count = h('div', { class: 'count-line' }, countText);
  const sentinel = h('div', { class: 'sentinel' });
  const fmtRow = libraryFormatRow();
  list.append(count, fmtRow, rows, sentinel);
  libObserver?.disconnect();
  libObserver = new IntersectionObserver((es) => { if (es.some((e) => e.isIntersecting) && L.rows.length && !L.error) loadLibrary(false); }, { root: list, rootMargin: '600px' });
  libObserver.observe(sentinel);

  const search = h('input', { type: 'search', placeholder: tx('搜索标题或正文'), value: L.query.text, 'aria-label': tx('搜索资料库'), enterkeyhint: 'search' });
  let composing = false; let timer;
  const update = () => { clearTimeout(timer); timer = setTimeout(() => { L.query.text = search.value; L.scroll = 0; loadLibrary(true); }, 250); };
  search.addEventListener('compositionstart', () => { composing = true; });
  search.addEventListener('compositionend', () => { composing = false; update(); });
  search.addEventListener('input', () => { if (!composing) update(); });

  const chip = connChip();
  const banner = h('div');
  const facets = libraryFacets();
  count.append(facets);
  const screen = h('div', { class: 'screen' }, ribbon(),
    h('header', { class: 'top topbar' },
      h('button', { class: 'icon-btn', 'aria-label': tx('打开资料库导航'), onclick: () => openSide('left') }, icon('menu', 22)),
      h('div', { class: 'brand serif' }, BRAND_NAME, h('span', { class: 'brand-ver' }, `v${APP_VERSION}`)), chip,
      h('button', { class: 'avatar-btn', 'aria-label': tx('账户与设置'), onclick: () => go('/settings') }, icon('user', 18))),
    h('div', { class: 'fixed-head' }, banner, h('label', { class: 'search' }, icon('search', 18), search), originControl()),
    list, tabbar('library'));
  screen.append(...batchSheet());
  lib = { screen, list, rows, banner, facets, chip, count, countText, fmtRow, rendered: 0 };
  requestAnimationFrame(() => {
    refreshLibrary(false);
    list.scrollTop = L.scroll;
    if (!L.view && !L.loading && !L.error) loadLibrary(true);
    // First window from the cache while the Mac answers; replaced by the live window.
    const q = L.query;
    if (!L.rows.length && !q.text && !q.source && !q.item_type && !q.group_id) {
      cacheKv('library').then((c) => {
        if (!c || L.rows.length || L.view) return;
        Object.assign(L, { rows: [...c.items], total: c.total, sources: c.sources, item_types: c.item_types, next: null, cached: true });
        refreshLibrary(true);
      });
    }
  });
  return screen;
}

// ---------- reader + chat (m1-2) ----------
// Original text arrives as ≤16KiB UTF-8 byte segments bound to the item revision; translation is
// separate derived text and its offsets are never used as original coordinates.
const SEGMENT = 16 * 1024;
// item.get has no item_type / captured_at / published_at / duration_seconds; take them from the query row when known.
function withRow(meta) {
  const row = state.lib.rows.find((r) => r.item_id === meta.item_id);
  return { ...meta, item_type: meta.item_type ?? row?.item_type ?? null, captured_at: meta.captured_at ?? row?.captured_at ?? null,
    published_at: meta.published_at ?? row?.published_at ?? null, duration_seconds: meta.duration_seconds ?? row?.duration_seconds ?? null,
    translated_title: meta.translated_title ?? row?.translated_title ?? null, title_source: meta.title_source ?? row?.title_source ?? null };
}
// A cached copy paints at once and is labelled until item.get confirms its revision; a new
// revision replaces it (and its entry); without the Mac it stays, marked 离线 · 缓存版本.
async function loadItem(id) {
  if (state.item?.id === id && !editGuard()) return;
  // Each item opens on the original unless the caller asked for its finished translation.
  if (state.item?.id !== id) { state.lang = state.openLang || 'orig'; state.openLang = null; state.insView = 'text'; }
  const it = { id, meta: null, text: '', nextByte: 0, total: null, loading: true, error: null, translation: null };
  state.item = it;
  render();
  const cached = await cacheGet(id);
  if (cached && state.item === it && !it.meta) {
    Object.assign(it, { meta: withRow(cached.meta), text: cached.text, nextByte: cached.nextByte, total: cached.total,
      translation: cached.translation && { ...cached.translation, read: true },
      minutes: cached.minutes && { ...cached.minutes, read: true }, cache: 'checking', cachedNext: cached.nextByte });
    const c = chatFor(id);
    if (cached.chat && !c.hist) { c.stored = cached.chat; c.hist = 'cached'; mergeHistory(c); }
    if (route().name === 'item') render();
  }
  try {
    const meta = withRow(await source.itemRead(id));
    if (state.item !== it) return;
    noteOutcome(null);
    // A paired book is read by chapter from its editions; its canonical Markdown (the
    // translation) is never shown as 原文 and the book is not kept in the article cache.
    const book = await readBook(meta);
    if (state.item !== it) return;
    const replaced = !!it.meta && meta.revision_id !== it.meta.revision_id;
    if (replaced) cacheDrop(id);
    if (!it.meta || replaced) {
      Object.assign(it, { meta, total: meta.original_total_bytes, text: '', nextByte: 0, translation: null, minutes: null });
      if (meta.first_segment) { it.text = meta.first_segment.text; it.nextByte = meta.first_segment.end_byte; }
    } else it.meta = meta; // same revision: keep the longer cached text
    it.cache = null;
    if (book) { it.book = book; it.noCache = true; cacheDrop(id); }
    if (!book) { lrOpen(it, plainDoc?.item_id === meta.item_id && plainDoc.revision_id === meta.revision_id ? plainDoc : null); plainDoc = null; }
    if (replaced) toast(tx('条目已在 Mac 上更新，已换成最新版本'));
    if (!book) loadTranslation();
    loadMinutes(); loadInsights(); loadHistory(id);
    if (book) { it.loading = false; if (route().name === 'item') render(); return; }
    if (!it.text && it.nextByte < it.total) await loadSegment();
    cacheSoon();
  } catch (e) {
    if (state.item !== it) return;
    noteOutcome(e);
    if (it.cache) it.cache = 'offline'; else it.error = e.code || 'unknown';
    const c = state.chats?.[id];
    if (c?.hist === 'cached') c.histErr = e.code || 'unknown';
  }
  it.loading = false;
  if (route().name === 'item') render();
}
async function loadSegment() {
  const it = state.item;
  if (!it || it.nextByte >= it.total) return;
  // The client cannot see byte boundaries; back off up to 3 bytes until the end is a valid UTF-8 boundary.
  for (let back = 0; back < 4; back += 1) {
    const end = Math.min(it.total, it.nextByte + SEGMENT) - back;
    try {
      const r = await source.contextRead({ id: it.id, expectedRevision: it.meta.revision_id, startByte: it.nextByte, endByte: end });
      it.text += r.text; it.nextByte = r.end_byte; it.total = r.total_bytes;
      cacheSoon();
      return;
    } catch (e) {
      if (e.code === 'invalid_context_range' && end < it.total) continue;
      if (e.code === 'stale_item_revision') { loadItem(it.id); return; }
      throw e;
    }
  }
}
// Translation: status first; text is read (server-paged) only when the job succeeded for this revision.
// Status, translated title and batch progress are read when the item opens (they show in the
// reader and the right sidebar) and followed while a job is queued or running.
async function loadTranslation() {
  const it = state.item;
  const id = it.id;
  try {
    const st = await source.translationStatus(id);
    noteOutcome(null);
    if (state.item !== it) return;
    // The status names another revision than the item shown (e.g. a cached item, or one that changed
    // on the Mac while open): read the item once more (meta, and the body if it is newer) and then the
    // status again. At most once per open of the item (the flag lives on this item object), so a
    // mismatch never turns into a reload loop; still different → 条目已更新 · 重新打开.
    if (st.revision_id && st.revision_id !== it.meta.revision_id) {
      if (!it.metaReread) { it.metaReread = true; await refreshMeta(it); if (state.item === it) await loadTranslation(); return; }
      throw Object.assign(new Error('stale_item_revision'), { code: 'stale_item_revision' });
    }
    const prev = it.translation;
    const keep = prev?.status === 'succeeded' && st.state === 'succeeded';
    if (!keep) it.translation = { status: st.state, reason: st.reason, text: '', next: st.state === 'succeeded' ? 0 : null, title: null };
    // Only status read from the Mac is cached (`read`), never a request's own reply.
    Object.assign(it.translation, { translatedTitle: st.translated_title ?? null, progress: st.progress ?? null, read: true });
    if (!keep && st.state === 'succeeded') await loadTranslationPage();
    cacheSoon();
  } catch (e) {
    if (state.item !== it) return;
    noteOutcome(e);
    // Unreachable: a cached translation stays readable rather than turning into an error.
    if (!(e.code === 'unavailable' && it.translation && !it.translation.error)) it.translation = { error: e.code || 'unknown' };
  }
  if (route().name === 'item' && state.item?.id === id) softRender();
  schedulePoll();
}
// The open item's details read again in place (no new item object, so the chat draft, sheets and the
// one-reread flag stay). A newer revision replaces the body exactly as opening it would: the cached
// copy of the older revision is dropped and the new one cached; same revision: only meta refreshes.
async function refreshMeta(it) {
  const meta = withRow(await source.itemRead(it.id));
  noteOutcome(null);
  if (state.item !== it) return;
  if (meta.revision_id === it.meta.revision_id) { it.meta = meta; return; }
  cacheDrop(it.id);
  Object.assign(it, { meta, total: meta.original_total_bytes, text: '', nextByte: 0, translation: null, minutes: null, cache: null, cachedNext: null });
  if (meta.first_segment) { it.text = meta.first_segment.text; it.nextByte = meta.first_segment.end_byte; }
  loadMinutes(); loadInsights();
  if (!it.text && it.nextByte < it.total) await loadSegment();
  if (state.item !== it) return;
  lrRevised(it);
  cacheSoon();
  if (route().name === 'item') render();
  toast(tx('条目已在 Mac 上更新，已换成最新版本')); // after the render, which would remove it
}
async function loadTranslationPage() {
  const it = state.item; const t = it.translation;
  if (!t || t.next == null) return;
  // Status loading and switching to 对照 may ask for the same page concurrently.
  // Share that read; never append one byte range twice or into a replacement item.
  if (t.reading) return t.reading;
  t.reading = (async () => {
    const r = await source.translationRead({ id: it.id, expectedRevision: it.meta.revision_id, startByte: t.next });
    if (state.item !== it || it.translation !== t) return;
    t.text += r.text; t.next = r.next_byte; t.title = r.title ?? t.title;
    cacheSoon();
  })();
  try { await t.reading; } finally { delete t.reading; }
}
// AI 纪要: manual only. The status read never starts anything; 生成纪要 is an explicit request.
async function loadMinutes() {
  const it = state.item;
  try { it.minutes = { ...(await source.minutesStatus(it.id)), read: true }; noteOutcome(null); cacheSoon(); } catch (e) {
    noteOutcome(e);
    if (!(e.code === 'unavailable' && it.minutes && !it.minutes.error)) it.minutes = { error: e.code || 'unknown' };
  }
  if (state.item === it && route().name === 'item') softRender();
  schedulePoll();
}
// An unknown outcome (reply lost after sending) is read back from the status; never resent.
async function requestTranslation() {
  const it = state.item;
  it.trAsk = 'requesting'; render();
  try {
    const r = await source.translationRequest(it.id);
    noteOutcome(null);
    it.trAsk = null;
    it.translation = { status: r.state, reason: r.reason, text: '', next: r.state === 'succeeded' ? 0 : null, title: null, translatedTitle: r.translated_title, progress: r.progress };
    if (r.state === 'succeeded') await loadTranslationPage();
  } catch (e) {
    noteOutcome(e);
    it.trAsk = { error: e.code || 'unknown' };
    if (e.code === 'unknown') { await loadTranslation(); return; }
  }
  if (state.item === it) render();
  schedulePoll();
}
async function requestMinutes() {
  const it = state.item;
  it.mAsk = 'requesting'; render();
  try { it.minutes = await source.minutesRequest(it.id); it.mAsk = null; noteOutcome(null); } catch (e) {
    noteOutcome(e);
    it.mAsk = { error: e.code || 'unknown' };
    if (e.code === 'unknown') { await loadMinutes(); return; }
  }
  if (state.item === it) render();
  schedulePoll();
}
const BUSY = new Set(['queued', 'running', 'leased', 'awaiting_enqueue']);
// What this Mac's gateway serves (memex.session.info), read once per pairing; a failed read is
// retried on the next item. Insights and the stored conversation appear only when served.
let capsP = null;
function caps() {
  if (!GATEWAY) return Promise.resolve(state.caps = { insights: 'available', conversation: 'available', reader: 'available', ...source.alignCaps() });
  capsP ??= source.sessionInfo().then((info) => (state.caps = info), (e) => { capsP = null; throw e; });
  return capsP;
}
const insightsOn = () => !GATEWAY || state.caps?.insights === 'available';
const historyOn = () => !GATEWAY || state.caps?.conversation === 'available';
// reader_v1 (paired books): only when served; a gateway that does not report it has none.
const readerOn = () => state.caps?.reader === 'available';
let itemPoll = null;
function schedulePoll() {
  clearTimeout(itemPoll);
  const it = state.item;
  const insBusy = INSIGHT_KINDS.some(([k]) => BUSY.has(it?.insights?.[k]?.state));
  if (!it || !(BUSY.has(it.translation?.status) || BUSY.has(it.minutes?.state) || insBusy)) return;
  itemPoll = setTimeout(() => {
    if (state.item !== it || route().name !== 'item') return;
    if (BUSY.has(it.translation?.status)) loadTranslation();
    if (BUSY.has(it.minutes?.state)) loadMinutes();
    if (insBusy) loadInsights();
  }, GATEWAY ? 4000 : 1200);
}
// A background status change must not take focus (and the keyboard) away from a text field.
let renderOnBlur = false;
function softRender() {
  // A selection in the reader (its marking bar is up) survives a background refresh until it ends.
  if (route().name === 'item' && lrSelRange()) { lrRenderLater = true; return; }
  if (dial) {
    // Status polling must not destroy a live pointer capture or the wheel position.
    document.querySelector('.ai-bar')?.replaceWith(aiBar(state.item));
    dial.needsRender = true;
    return;
  }
  const a = document.activeElement;
  if (a && /^(TEXTAREA|INPUT)$/.test(a.tagName) && app.contains(a)) {
    if (!renderOnBlur) { renderOnBlur = true; a.addEventListener('blur', () => { renderOnBlur = false; render(); }, { once: true }); }
    return;
  }
  render();
}
// Loads the rest of the original and of the translation (bounded) so 对照 can pair whole documents.
async function loadFull() {
  const it = state.item;
  if (it.fullLoading) return;
  it.fullLoading = true;
  try {
    for (let i = 0; i < 128 && state.item === it && it.nextByte < it.total; i += 1) await loadSegment();
    for (let i = 0; i < 128 && state.item === it && it.translation?.next != null; i += 1) await loadTranslationPage();
    noteOutcome(null);
  } catch (e) { noteOutcome(e); toast(ERR_TEXT[e.code] || tx('载入失败')); it.fullFailed = true; }
  it.fullLoading = false;
  if (state.item === it && route().name === 'item') softRender();
}

// ---------- paired books (reader_v1) ----------
// A book has an `original` and a `translation` edition, each a list of chapters. 原文 shows the
// original edition's chapter, 译文 the translation edition's; switching follows the declared
// counterpart key (chapter correspondence only, never byte offsets, scroll fractions or paragraph
// indexes). Chapters are read in bounded chunks (next_offset followed to the chapter's end); only
// the shown chapters and the prefetched next and previous chapters are kept (LRU of BOOK_KEEP).
const BOOK_LIMIT = 32768;
const BOOK_KEEP = 6;
const BOOK_ERR = {
  stale_book_manifest: tx('书的版本已变化，请刷新'), stale_item_revision: tx('条目已在 Mac 上更新，请刷新'),
  library_resource_not_found: tx('条目不存在或无权访问'), reader_side_not_found: tx('Mac 上找不到这本书的这一版本，请刷新'),
  reader_chapter_not_found: tx('Mac 上找不到这一章，请刷新'), invalid_reader_offset: tx('阅读位置无效，请刷新'),
  invalid_reader_request: tx('阅读请求不被这台 Mac 接受'), invalid_reader_limit: tx('阅读请求不被这台 Mac 接受'),
  unavailable: tx('无法连接到你的 Mac'), invalid_response: tx('Mac 返回的书籍数据不完整'),
  library_loading: BUSY_TEXT,
};
const bookErr = (code) => BOOK_ERR[code] || tx('书籍读取失败：{0}', code);
// A read bound to an older revision of the item: the book is read again at the current one.
const BOOK_STALE = new Set(['stale_item_revision', 'stale_book_manifest']);
// Reads the item and its book again (keeping chapter, mode, every draft and the chapter editor),
// at most once per revision so a persistent error is still shown. False when not started.
function bookStale(it) {
  if (state.item !== it || it.bookReloading || it.staleTried === it.meta?.revision_id) return false;
  const from = it.meta.revision_id;
  it.bookReloading = true; it.staleTried = from;
  reloadBook(it).then(() => { noteOutcome(null); if (it.meta.revision_id !== from) toast(tx('条目已在 Mac 上更新，已换成最新版本')); },
    (e) => { if (e.code !== 'cancelled') { noteOutcome(e); toast(bookErr(e.code || 'unknown')); } })
    .finally(() => { it.bookReloading = false; if (state.item === it && route().name === 'item') softRender(); });
  return true;
}
// Errors a retry cannot fix: the item, its manifest or its chapters changed; reopen it instead.
const BOOK_REFRESH = new Set(['stale_book_manifest', 'stale_item_revision', 'reader_side_not_found', 'reader_chapter_not_found', 'invalid_reader_offset']);
const LANG_NAMES = { en: 'English', zh: '中文', 'zh-hans': '中文', 'zh-cn': '中文', ja: '日本語', fr: 'Français', de: 'Deutsch', ru: 'Русский', es: 'Español' };
const langName = (code) => LANG_NAMES[String(code || '').toLowerCase()] || code || '';
// null: not a book (ordinary item, or this Mac serves no reader); { error }: unknown, shown honestly.
let plainDoc = null; // the last ordinary document's toc read by readBook (picked up by loadItem)
async function readBook(meta) {
  try { await caps(); } catch { /* session.info unreachable: treated as no reader, like an older gateway */ }
  if (!readerOn()) return null;
  let doc;
  try { doc = await source.readerToc({ item_id: meta.item_id, revision_id: meta.revision_id }); } catch (e) {
    noteOutcome(e);
    // A gateway that advertises the reader but rejects the route behaves like one without it.
    if (e.code === 'invalid_request') return null;
    return { error: e.code || 'unknown' };
  }
  const o = doc.editions.find((e) => e.side === 'original');
  const t = doc.editions.find((e) => e.side === 'translation');
  // An ordinary document: its `source` edition (sha256, section-* chapters) anchors 阅读位置 / 手工标记.
  if (!o?.chapters.length || !t?.chapters.length) { plainDoc = doc; return null; }
  return { doc, o, t, pos: { side: 'original', key: o.chapters[0].key }, store: new Map(), scrolls: new Map(), picker: false, align: new Map(), ai: new Map() };
}
const bookEd = (b, side) => (side === 'original' ? b.o : b.t);
const bookCh = (b, side, key) => bookEd(b, side).chapters.find((c) => c.key === key) || null;
// The chapter shown on `side` for the current position: itself, its counterpart, or null (none).
function bookChapter(b, side) {
  const cur = bookCh(b, b.pos.side, b.pos.key);
  if (!cur) return null;
  return b.pos.side === side ? cur : cur.counterpart ? bookCh(b, side, cur.counterpart) : null;
}
// Chapter order for the picker and ‹ ›: one edition's chapters, or (对照) the original's with the
// translation-only chapters (front matter) placed where they occur, so every chapter stays reachable.
function bookNav(b, lang) {
  if (lang === 'orig') return b.o.chapters.map((c) => ({ side: 'original', c }));
  if (lang === 'trans') return b.t.chapters.map((c) => ({ side: 'translation', c }));
  const out = []; const oc = b.o.chapters; let i = 0;
  for (const c of b.t.chapters) {
    const j = c.counterpart ? oc.findIndex((x) => x.key === c.counterpart) : -1;
    if (j < 0) { out.push({ side: 'translation', c }); continue; }
    for (; i <= j; i += 1) out.push({ side: 'original', c: oc[i] });
  }
  for (; i < oc.length; i += 1) out.push({ side: 'original', c: oc[i] });
  return out;
}
function bookIndex(b, nav, lang) {
  const side = lang === 'trans' ? 'translation' : 'original';
  const c = bookChapter(b, side);
  return c ? nav.findIndex((n) => n.side === side && n.c.key === c.key) : nav.findIndex((n) => n.side === b.pos.side && n.c.key === b.pos.key);
}
// [side, chapter] pairs on screen for the current mode.
function bookShown(b) {
  const o = bookChapter(b, 'original'); const t = bookChapter(b, 'translation');
  if (state.lang === 'orig') return o ? [['original', o]] : [];
  if (state.lang === 'trans') return t ? [['translation', t]] : [];
  return [o && ['original', o], t && ['translation', t]].filter(Boolean);
}
function bookGo(it, n) {
  if (!n || !editGuard()) return;
  it.book.pos = { side: n.side, key: n.c.key }; it.book.picker = false;
  render();
}
// 下一章 (the › button and the chapter-end footer): the next chapter from its top.
function bookNext(it, n) {
  if (!n || !editGuard()) return;
  const b = it.book;
  b.pos = { side: n.side, key: n.c.key }; b.picker = false;
  for (const [side, c] of bookShown(b)) { b.scrolls.delete(`${side}:${c.key}`); b.scrolls.delete(`${side}:${c.key}|v`); }
  b.scrolls.delete(`ai:${bookChapter(b, 'original')?.key}|v`);
  const o = bookChapter(b, 'original');
  if (o) b.scrolls.delete(`al:${o.key}`);
  render();
}
// The end of a chapter's text: 下一章 · {title}, or 全书完 after the last chapter. Never auto-jumps.
function bookEnd(it) {
  const b = it.book; const nav = bookNav(b, state.lang); const next = nav[bookIndex(b, nav, state.lang) + 1];
  return h('div', { class: 'book-end' }, next
    ? h('button', { class: 'book-next', onclick: () => bookNext(it, next) }, tx('下一章 · {0}', next.c.title))
    : h('span', { class: 'book-fin' }, tx('全书完')));
}
function trimBook(b, keep) {
  for (const k of b.store.keys()) {
    if (b.store.size <= BOOK_KEEP) break;
    if (!keep.has(k) && !b.store.get(k).loading) b.store.delete(k);
  }
}
function loadBookChapter(it, side, ch) {
  const b = it.book; const k = `${side}:${ch.key}`;
  if (b.store.has(k)) return b.store.get(k).ready;
  const e = { text: '', done: false, error: null, loading: true };
  b.store.set(k, e);
  e.ready = readBookChapter(it, b, side, ch, e);
  return e.ready;
}
async function readBookChapter(it, b, side, ch, e) {
  const ed = bookEd(b, side);
  const parts = []; let offset = 0; let background = false;
  try {
    for (;;) {
      // A prefetch reads only while no foreground read is in flight, and never retries; once the
      // chapter is shown (promoted) its remaining pieces are ordinary reads.
      background = !!e.background;
      if (background) { await source.idle?.(); background = !!e.background; if (it.book !== b || state.item !== it) return; }
      const r = await source.readerChunk({ item_id: it.id, revision_id: it.meta.revision_id, side, chapter: ch.key, offset, limit: BOOK_LIMIT }, { background });
      if (it.book !== b || state.item !== it) return;
      // Exactly the requested piece of this chapter, in order, ending at the chapter's end.
      const end = r.next_offset == null ? ch.end : ch.start + r.next_offset;
      if (r.start !== ch.start + offset || r.end !== end || (ed.sha256 && r.sha256 && r.sha256 !== ed.sha256)) throw Object.assign(new Error('invalid_response'), { code: 'invalid_response' });
      parts.push(r.markdown);
      if (r.next_offset == null) break;
      offset = r.next_offset;
    }
    e.text = parts.join(''); e.done = true; noteOutcome(null);
  } catch (err) {
    // The item moved on (e.g. its title changed on the Mac): read the book again, no error shown.
    if (BOOK_STALE.has(err.code) && bookStale(it)) return; // stays 正在载入本章… until the new book replaces it
    if (background) {
      // A prefetch that met a busy Mac or a dropped link is abandoned silently (no outcome, no
      // retry); if the chapter is on screen by now it is read again as an ordinary read.
      e.loading = false; e.abandoned = true;
      if (b.store.get(`${side}:${ch.key}`) === e) b.store.delete(`${side}:${ch.key}`);
      if (!e.background && state.item === it && route().name === 'item') softRender();
      return;
    }
    noteOutcome(err); e.error = err.code || 'unknown';
  }
  e.loading = false;
  // A prefetched chapter that is not on screen changes nothing visible: no re-render (it would
  // replace the panes under the reader's finger).
  if (e.background) return;
  if (state.item === it && route().name === 'item') softRender();
}
// Loads what is on screen; once it is in, prefetches the next and then the previous chapter in
// the background (same mode: the sides bookShown gives at that position), one read at a time and
// only while no foreground read is in flight. Text only (chunk reads): no translation, alignment or
// model request, and the position and scroll are left untouched. A busy Mac or a dropped link
// abandons the prefetch for this position (never retried). BOOK_KEEP (6) holds the worst case,
// 对照: 2 shown + 2 next + 2 previous.
function ensureBook(it) {
  const b = it.book;
  const shown = bookShown(b);
  const keep = new Set(shown.map(([side, c]) => `${side}:${c.key}`));
  for (const k of keep) { const e = b.store.get(k); if (e) { e.background = false; b.store.delete(k); b.store.set(k, e); } } // recently used; promoted
  for (const [side, c] of shown) loadBookChapter(it, side, c);
  if (shown.every(([side, c]) => b.store.get(`${side}:${c.key}`)?.done)) {
    const nav = bookNav(b, state.lang); const at = bookIndex(b, nav, state.lang);
    const save = b.pos; const queue = [];
    for (const n of [nav[at + 1], at > 0 ? nav[at - 1] : null]) {
      if (!n) continue;
      b.pos = { side: n.side, key: n.c.key };
      for (const [side, c] of bookShown(b)) { keep.add(`${side}:${c.key}`); queue.push([side, c]); }
    }
    b.pos = save;
    prefetchBook(it, b, queue);
  }
  trimBook(b, keep);
}
async function prefetchBook(it, b, queue) {
  const key = queue.map(([side, c]) => `${side}:${c.key}`).join(',');
  if (!key || b.prefetchKey === key) return; // running, done or abandoned for this position
  b.prefetchKey = key;
  for (const [side, c] of queue) {
    if (it.book !== b || b.prefetchKey !== key || state.item !== it) return;
    const k = `${side}:${c.key}`;
    let e = b.store.get(k);
    if (!e) {
      e = { text: '', done: false, error: null, loading: true, background: true };
      b.store.set(k, e);
      e.ready = readBookChapter(it, b, side, c, e);
    }
    await e.ready;
    if (e.abandoned || e.error) return;
  }
}
function bookBlocks(md) {
  return splitBlocks(md).flatMap((b) => {
    const els = b.fenced ? [h('pre', { class: 'code' }, fenceBody(b.text))] : mdBlock(b.text, { zh: curZh(), tcy: vertNow(state.item) });
    if (!b.fenced && isNoteText(b.text)) for (const el of els) el.classList.add('lnote');
    return els;
  });
}
// 注释: a block whose text starts with a circled number ①–⑳ or a Markdown footnote definition
// `[^x]:`, or a paragraph-index row of kind 'note'. Drawn one size smaller, muted, in the 注释字体
// of 设置 › 外观; the first note of each consecutive run gets a divider above it.
const NOTE_START = /^\s*(?:[\u2460-\u2473]|\[\^[^\]\s]+\]:)/;
const isNoteText = (t) => NOTE_START.test(t || '');
const isNoteRow = (r) => r.kind === 'note' || [r.o, r.t].filter((x) => x != null).every(isNoteText);
function markNoteRuns(container) {
  let prev = false;
  for (const el of container.children) { const n = el.classList.contains('lnote'); el.classList.toggle('lnote-first', n && !prev); prev = n; }
  return container;
}
const applyNoteFont = () => { document.documentElement.dataset.noteFont = state.noteFont; };
applyNoteFont();
function bookPane(it, side, ch, { primary = false, lead = null, rows = null, end = primary } = {}) {
  const b = it.book; const e = b.store.get(`${side}:${ch.key}`);
  const retry = () => {
    if (BOOK_REFRESH.has(e.error)) { loadItem(it.id); return; }
    b.store.delete(`${side}:${ch.key}`);
    // Busy or out of reach: only what failed is read again; a checked paragraph index stays.
    if (e.error === MAC_BUSY || e.error === 'unavailable') { for (const [k, a] of b.align) if (a.state === 'error') b.align.delete(k); } else b.align.clear();
    render();
  };
  const s = side === 'original' ? 'o' : 't';
  // With a checked paragraph index the chapter is drawn row by row (same bytes), so the synced
  // 并排 can find each matched row's start boundary in both panes.
  const kids = rows ? [markNoteRuns(h('div', { class: 'book-text', lang: bookEd(b, side).language || null }, rows.filter((r) => r[s] != null)
    .map((r) => markNoteRuns(h('div', { class: `al-seg${r.kind === 'note' ? ' al-note' : ''}${r.kind === 'note' || isNoteText(r[s]) ? ' lnote' : ''}`, 'data-row': String(r.i), 'data-rel': r.relation }, bookBlocks(r[s])))))), end ? bookEnd(it) : null]
    : e?.done ? [markNoteRuns(h('div', { class: 'book-text', lang: bookEd(b, side).language || null }, bookBlocks(e.text))), end ? bookEnd(it) : null]
    : e?.error ? [h('div', { class: 'note', 'data-busy': e.error === MAC_BUSY ? '1' : null }, bookErr(e.error), ' ', h('button', { class: 'linkish', onclick: retry }, BOOK_REFRESH.has(e.error) ? tx('刷新') : tx('重试')))]
      : [waitNote(tx('正在载入本章…'))];
  // CSSOM, not a style attribute: the gateway's CSP (style-src 'self') refuses inline style attributes.
  const article = h('article', { class: `article${fontCls(it)}` }, lead, ...kids);
  article.style.setProperty('--reading-size', `${state.readingSize}px`);
  const pane = h('div', { class: `scroll book-pane${primary && vertNow(it) ? ' vert' : ''}`, id: primary ? 'reader-scroll' : null, 'data-item': it.id, 'data-pane': `${side}:${ch.key}` }, article);
  if (pane.classList.contains('vert')) bindVert(pane);
  if (primary) pane.addEventListener('scroll', () => { if (!scrollQueued) { scrollQueued = true; requestAnimationFrame(() => { scrollQueued = false; updateReaderChrome(); }); } }, { passive: true });
  return pane;
}
// The note shown where a chapter has no counterpart on the requested side.
function bookMissing(it, side, primary) {
  const other = side === 'original' ? 'trans' : 'orig';
  return h('div', { class: 'scroll book-pane', id: primary ? 'reader-scroll' : null, 'data-item': it.id },
    h('article', { class: 'article' }, h('div', { class: 'note book-missing' },
      side === 'translation' ? tx('此章只有原文，没有对应的译文章节。') : tx('此章只有译文，没有对应的原文章节。'), ' ',
      h('button', { class: 'linkish', onclick: () => { if (!editGuard()) return; state.lang = other; render(); } }, side === 'translation' ? tx('看原文') : tx('看译文')))));
}
function bookBar(it) {
  const b = it.book;
  if (!b.doc || state.insView !== 'text') return null;
  const nav = bookNav(b, state.lang); const idx = bookIndex(b, nav, state.lang);
  const side = state.lang === 'trans' ? 'translation' : 'original';
  const cur = bookChapter(b, side) || bookCh(b, b.pos.side, b.pos.key);
  // An AI translation shown in 对照 without a recorded correspondence: no 段落 / 并排 sync to offer.
  const oc = bookChapter(b, 'original'); const ai = oc && !bookChapter(b, 'translation') ? b.ai.get(oc.key) : null;
  const noMap = state.lang === 'bi' && ai?.state === 'ready' && !!ai.map && ai.map.state !== 'ready';
  return h('nav', { class: 'book-bar', 'aria-label': tx('章节') },
    h('button', { class: 'icon-btn', 'aria-label': tx('上一章'), disabled: idx <= 0, onclick: () => bookGo(it, nav[idx - 1]) }, icon('back', 20)),
    h('button', { class: 'book-ch', 'aria-haspopup': 'dialog', 'aria-label': tx('章节：{0}（点击选择）', cur?.title || ''), onclick: () => { b.picker = true; render(); } },
      h('span', { class: 'grow' }, zhShow(cur?.title || '')), h('span', { class: 'book-count' }, `${idx >= 0 ? idx + 1 : '–'}/${nav.length}`), icon('down', 14)),
    h('button', { class: 'icon-btn', 'aria-label': tx('下一章'), disabled: idx >= nav.length - 1, onclick: () => bookNext(it, nav[idx + 1]) }, icon('chev', 20)),
    state.lang === 'bi' ? h('div', { class: 'seg3 book-lay', role: 'group', 'aria-label': tx('对照方式') }, [['para', tx('段落')], ['side', tx('并排')]].map(([k, label]) => h('button', {
      class: state.bookLayout === k ? 'on' : '', 'aria-pressed': String(state.bookLayout === k), 'data-layout': k, disabled: noMap,
      onclick: () => { state.bookLayout = k; localStorage.setItem('memex.m.bookLayout', k); render(); },
    }, label))) : null);
}
function bookBody(it) {
  const b = it.book;
  if (!b.doc) {
    return h('div', { class: 'reader-body' }, h('div', { class: 'empty book-error' }, bookErr(b.error),
      h('div', {}, h('button', { class: 'btn-sm mt', onclick: () => loadItem(it.id) }, BOOK_REFRESH.has(b.error) ? tx('刷新') : tx('重试')))));
  }
  if (state.insView !== 'text') {
    // 金句 / 事实核查 replace the body, as for any item.
    const pane = h('div', { class: 'scroll', id: 'reader-scroll', 'data-item': it.id }, h('article', { class: 'article' }, state.insView === 'quotes' ? quotesPanel(it) : factsPanel(it)));
    return h('div', { class: 'reader-body' }, pane);
  }
  ensureBook(it);
  const o = bookChapter(b, 'original'); const t = bookChapter(b, 'translation');
  let body; let cls = state.lang;
  // 翻译本章: only an original chapter without a counterpart (never one that has a 译文).
  const ai = o && !t ? aiFor(it, b, o) : null;
  const aiReady = ai?.state === 'ready';
  if (aiReady) aimEnsure(it);
  const xp = it.xlate;
  if (xp?.phase === 'polling' && !xp.timer && xp.chapter === o?.key) pollXlate(it, xp);
  if (state.lang !== 'bi') {
    const side = state.lang === 'trans' ? 'translation' : 'original'; const ch = side === 'original' ? o : t;
    const pane = ch ? bookPane(it, side, ch, { primary: true, lead: vertNow(it) ? null : minutesCard(it) })
      : side === 'translation' && aiReady ? aiPane(it, o, ai, { primary: true, lead: true })
        : side === 'translation' && ['checking', 'reading'].includes(ai?.state) ? h('div', { class: 'scroll book-pane', id: 'reader-scroll', 'data-item': it.id }, h('article', { class: 'article' }, waitNote(tx('正在载入 AI 译文…'))))
          : bookMissing(it, side, true);
    body = [o && !t ? xlateBanner(it, o, ai, side) : null, pane, DRAFTS && !pane.classList.contains('vert') ? scrubberEl() : null];
  } else {
    // 对照: paragraph-interleaved from the Mac's checked paragraph index (段落, default), or the two
    // counterpart chapters top/bottom (并排), synced at matched row boundaries when there is an index.
    const al0 = o && t && alignOn() ? alignFor(it, b, o, t) : null;
    const al = al0?.state === 'off' ? null : al0;
    const ready = al?.state === 'ready' && al.status !== 'none';
    const pending = al?.state === 'loading' || al?.state === 'check';
    if (o && t) restoreRepair(it, o, al);
    const rp = it.repair;
    if (rp?.phase === 'polling' && !rp.timer && rp.chapter === o?.key) pollRepair(it, rp);
    const para = state.bookLayout === 'para';
    // An original-only chapter with an AI translation: its recorded block correspondence (if any)
    // drives 段落 and the synced 并排 exactly like a human index; without one the text stays readable.
    const m = o && !t && aiReady ? ai.map : null;
    const mapped = m?.state === 'ready';
    const mapNote = (text, cls = '') => h('div', { class: `book-note ai-map-note${cls}`, role: 'status', 'data-map': m?.state || 'none' },
      h('span', { class: 'ai-trans-badge' }, tx('AI 译文')), h('span', { class: 'grow' }, text));
    if (o && !t && aiReady && mapped && para) {
      cls = 'para';
      body = [xlateBanner(it, o, ai, 'bi'), mapNote(tx('按翻译分块对照，未经人工核对')), h('div', { class: 'book-sync-hint ai-src' }, attribution(ai.translation?.route)), alignParaPane(it, o, { rows: m.rows, ai }, 'zh-CN'), DRAFTS ? scrubberEl() : null];
    } else if (o && !t && aiReady) {
      const orig = h('section', { class: 'book-half original', 'aria-label': tx('原文 · {0}', langName(b.o.language)) },
        h('div', { class: 'book-label' }, tx('原文 · {0}', langName(b.o.language))), bookPane(it, 'original', o, { primary: true, rows: mapped ? m.rows : null, end: false }));
      const aiHalf = h('section', { class: 'book-half translation ai', 'aria-label': tx('AI 译文') }, aiCaption(ai, 'book-label ai'), aiPane(it, o, ai, { end: true, rows: mapped ? m.rows : null }));
      const note = !m ? null : mapped ? mapNote(tx('按翻译分块对照，未经人工核对')) : m.state === 'loading' ? mapNote(tx('正在读取段落对应…'))
        : m.state === 'missing' ? mapNote(tx('这份 AI 译文没有保存逐段对照')) : mapNote(alignErr(m.error), ' bad');
      body = [xlateBanner(it, o, ai, 'bi'), note, mapped ? h('div', { class: 'book-sync-hint' }, tx('两侧按已匹配段落跟随滚动')) : null, h('div', { class: 'book-split' }, [orig, aiHalf])];
      if (mapped) bindSync(orig.querySelector('.book-pane'), aiHalf.querySelector('.book-pane'));
    } else if (o && t && para && ready) {
      cls = 'para';
      body = [alignBanner(it, o, al, al.status === 'partial' ? tx('这一章部分段落未匹配') : tx('段落对应由 AI 整理，未经核对')), alignParaPane(it, o, al), DRAFTS ? scrubberEl() : null];
    } else if (o && t && para && pending) {
      cls = 'para';
      body = [h('div', { class: 'scroll book-pane', id: 'reader-scroll', 'data-item': it.id }, h('article', { class: 'article' }, h('div', { class: 'note' }, tx('正在读取段落对应…'))))];
    } else {
      const half = (side, ch, primary) => h('section', { class: `book-half ${side}`, 'aria-label': side === 'original' ? tx('原文 · {0}', langName(b.o.language)) : tx('译文 · {0}', langName(b.t.language)) },
        h('div', { class: 'book-label' }, side === 'original' ? tx('原文 · {0}', langName(b.o.language)) : tx('译文 · {0}', langName(b.t.language))),
        bookPane(it, side, ch, { primary, rows: ready ? al.rows : null, end: primary && !(side === 'original' && aiReady) }));
      const text = !al ? (para ? tx('这一章还没有段落对照') : tx('这一章还没有段落对应，两侧暂不跟随滚动'))
        : al.state === 'error' ? alignErr(al.error)
          : pending ? tx('正在读取段落对应…')
            : ready ? (al.status === 'partial' ? tx('这一章部分段落未匹配') : tx('段落对应由 AI 整理，未经核对'))
              : para ? tx('这一章还没有段落对照') : tx('这一章还没有段落对应，两侧暂不跟随滚动');
      // The chapter footer (下一章 / 全书完) ends the AI translation, not the original above it.
      const aiHalf = o && !t && aiReady ? h('section', { class: 'book-half translation ai', 'aria-label': tx('AI 译文') },
        aiCaption(ai, 'book-label ai'), aiPane(it, o, ai, { end: true })) : null;
      const halves = [o ? half('original', o, true) : null, t ? half('translation', t, !o) : aiHalf];
      const xb = o && !t ? xlateBanner(it, o, ai, 'bi') : null;
      body = [o && t ? alignBanner(it, o, al, text) : aiReady ? null : h('div', { class: 'book-note' }, tx('此章没有对应章节')), xb,
        o && t && ready ? h('div', { class: 'book-sync-hint' }, tx('两侧按已匹配段落跟随滚动')) : null, h('div', { class: 'book-split' }, halves)];
      if (o && t && ready) bindSync(...halves.map((x) => x.querySelector('.book-pane')));
    }
  }
  // A tap on a saved 译文标记 (no selection) opens its bar: another style, or 删除.
  return h('div', { class: `reader-body book ${cls}`, onclick: (e) => {
    const mk = e.target.closest?.('[data-aib] mark.um[data-um]');
    if (mk && !lrSelRange()) lrMarkBar(it, mk.dataset.um, mk, aim(it));
  } }, editBanner(it), fmtBanner(it), body);
}
// ---------- paragraph correspondence (reader_alignment, frozen reader_v1 seam) ----------
// The Mac's paragraph index of an ORIGINAL chapter: pair rows whose chapter-relative UTF-8 byte
// ranges partition both counterpart chapters. All pages are read under one version (a changed index
// restarts from page 0), then checked against the chapter text read through chunk: exact partition
// of each side, row shapes, status, and both chapter SHA-256s. Any violation means no index, shown
// honestly; nothing is ever guessed from paragraph counts or scroll fractions.
const alignOn = () => state.caps?.reader_alignment === 'available';
// 整理本章段落 needs the repair capability and this pairing's edit grant; never from `reader` alone.
const repairOn = () => state.caps?.reader_repair === 'available' && !!state.caps?.scopes?.includes('edit');
const ALIGN_LIMIT = 256;
const ALIGN_RESTARTS = 3;
const U8E = new TextEncoder();
const U8D = new TextDecoder('utf-8', { fatal: true });
const codeErr = (code) => Object.assign(new Error(code), { code });
const sha256Hex = async (bytes) => [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))].map((x) => x.toString(16).padStart(2, '0')).join('');
const SAME_PAGE = ['version', 'translation_chapter', 'original_sha256', 'translation_sha256', 'original_bytes', 'translation_bytes', 'status', 'total'];
const ALIGN_ERR = {
  invalid_alignment: tx('段落对照与本章正文不一致，暂不使用'),
  invalid_reader_alignment_request: tx('段落对照请求不被这台 Mac 接受'),
  stale_chapter_alignment: tx('这一章的段落对应刚刚变化，请重新载入'),
  reader_job_in_progress: tx('这本书的另一章正在整理，请等它完成后再试'),
  reader_job_not_found: tx('Mac 上找不到这次整理任务'),
  no_low_dial_provider: tx('Mac 上还没有可用的 Low 档位模型，无法整理'),
  reader_repair_budget_exceeded: tx('这一章超出了整理的请求预算，本章没有改动'),
  reader_repair_failed: tx('整理没有成功，本章没有改动'),
  request_identity_conflict: tx('这次整理请求与之前的记录不一致，请重新发起'),
  provider_outcome_unknown: tx('不确定模型请求是否完成，本章没有改动'),
  stale_item_revision: tx('条目已在 Mac 上更新，请刷新'),
  permission_denied: tx('这次配对没有编辑权限，不能整理'),
  unknown: tx('没有收到 Mac 的回复，不确定整理是否已经开始'),
};
const alignErr = (code) => ALIGN_ERR[code] || bookErr(code);
// Every page of one chapter's index under one version; stale_chapter_alignment restarts (bounded).
async function fetchAlign(it, b, key) {
  for (let attempt = 0; attempt <= ALIGN_RESTARTS; attempt += 1) {
    let first = null; const pairs = []; let offset = 0; let stale = false;
    for (let page = 0; page < 4096; page += 1) {
      let r;
      try {
        r = await source.readerAlignment({ item_id: it.id, revision_id: it.meta.revision_id, chapter: key, offset, limit: ALIGN_LIMIT, expected_version: first ? first.version : 'any' });
      } catch (e) { if (e.code === 'stale_chapter_alignment') { stale = true; break; } throw e; }
      if (it.book !== b) throw codeErr('cancelled');
      if (r.offset !== offset || (first && SAME_PAGE.some((k) => r[k] !== first[k]))) throw codeErr('invalid_alignment');
      first ??= r; pairs.push(...r.pairs);
      if (r.next_offset == null) break;
      if (!(r.next_offset > offset) || r.version === 'missing') throw codeErr('invalid_alignment');
      offset = r.next_offset;
    }
    if (stale) continue;
    if (!first || pairs.length !== first.total) throw codeErr('invalid_alignment');
    return { ...first, pairs };
  }
  throw codeErr('stale_chapter_alignment');
}
// Rows with their text sliced by UTF-8 bytes, or null on any violation of the contract.
function alignRows(r, oText, tText, tKey) {
  const ob = U8E.encode(oText); const tb = U8E.encode(tText);
  if (r.translation_chapter !== tKey || r.original_bytes !== ob.length || r.translation_bytes !== tb.length) return null;
  const at = { o: 0, t: 0 }; const bytes = { o: ob, t: tb }; const rows = [];
  const shape = { matched: [true, true], original_only: [true, false], translation_only: [false, true] };
  try {
    for (const [i, p] of r.pairs.entries()) {
      const want = shape[p.relation];
      if (!p.o && !p.t) return null;
      if (want ? want[0] !== !!p.o || want[1] !== !!p.t : !!p.o === !!p.t) return null; // unresolved: exactly one side
      const row = { i, kind: p.kind, relation: p.relation, o: null, t: null };
      for (const s of ['o', 't']) {
        if (!p[s]) continue;
        const [a, z] = p[s];
        if (a !== at[s] || z <= a || z > bytes[s].length) return null; // monotonic, no gap, no overlap
        row[s] = U8D.decode(bytes[s].subarray(a, z)); // fatal: a split codepoint throws
        at[s] = z;
      }
      rows.push(row);
    }
  } catch { return null; }
  if (at.o !== ob.length || at.t !== tb.length) return null;
  const para = rows.some((x) => x.relation === 'matched' && x.kind === 'paragraph'); const open = rows.some((x) => x.relation === 'unresolved');
  if (r.status !== (!para ? 'none' : open ? 'partial' : 'aligned')) return null;
  return { rows, ob, tb };
}
// The checked index of original chapter `o` (state loading | check | ready | none | error), started once.
function alignFor(it, b, o, t) {
  let a = b.align.get(o.key);
  if (a) return a;
  a = { state: 'loading' }; b.align.set(o.key, a);
  a.ready = (async () => {
    try {
      const r = await fetchAlign(it, b, o.key);
      if (r.version === 'missing' || r.status === 'none') Object.assign(a, { state: 'none', version: r.version, status: 'none' });
      else {
        a.state = 'check';
        await Promise.all([loadBookChapter(it, 'original', o), loadBookChapter(it, 'translation', t)]);
        const eo = b.store.get(`original:${o.key}`); const et = b.store.get(`translation:${t.key}`);
        if (!eo?.done || !et?.done) throw codeErr(eo?.error || et?.error || 'cancelled');
        const v = alignRows(r, eo.text, et.text, t.key);
        if (!v) throw codeErr('invalid_alignment');
        // The hash check needs WebCrypto (secure contexts); without it only the partition is checked.
        if (globalThis.crypto?.subtle && ((await sha256Hex(v.ob)) !== r.original_sha256 || (await sha256Hex(v.tb)) !== r.translation_sha256)) throw codeErr('invalid_alignment');
        Object.assign(a, { state: 'ready', version: r.version, status: r.status, rows: v.rows });
      }
      noteOutcome(null);
    } catch (e) {
      if (BOOK_STALE.has(e.code) && bookStale(it)) return;
      if (e.code !== 'cancelled') noteOutcome(e);
      // A gateway that advertises the index but rejects the route behaves like one without it.
      Object.assign(a, e.code === 'invalid_request' ? { state: 'off' } : { state: 'error', error: e.code || 'unknown' });
    }
    if (it.book !== b) return;
    repairSettled(it, o.key);
    if (state.item === it && route().name === 'item') softRender();
  })();
  return a;
}
// 段落: each row in order. Matched rows show the original (smaller, muted) then the translation;
// one-sided rows carry 仅原文 / 仅译文, unresolved ones 未匹配 (never shown as a pair); notes stay notes.
function alignParaPane(it, o, a, tLang = null) {
  const b = it.book;
  // An AI translation (a.ai) draws its side with the source map and saved 译文标记.
  const side = (s, text, r) => h('div', { class: s === 'o' ? 'al-o' : 'al-t', lang: (s === 'o' ? b.o.language : tLang || b.t.language) || null },
    s === 't' && a.ai ? aiBlocks(it, o, a.ai, text, r.tb) : bookBlocks(text));
  const rows = a.rows.map((r) => {
    const tag = { original_only: tx('仅原文'), translation_only: tx('仅译文'), unresolved: tx('未匹配') }[r.relation];
    const tags = [r.kind === 'note' ? h('span', { class: 'al-tag note' }, tx('注')) : null, tag ? h('span', { class: `al-tag ${r.relation}` }, tag) : null].filter(Boolean);
    return h('div', { class: `al-row${r.kind === 'note' ? ' al-note' : ''}${isNoteRow(r) ? ' lnote' : ''}`, 'data-row': String(r.i), 'data-rel': r.relation, 'data-kind': r.kind },
      tags.length ? h('div', { class: 'al-tags' }, tags) : null, r.o != null ? markNoteRuns(side('o', r.o, r)) : null, r.t != null ? markNoteRuns(side('t', r.t, r)) : null);
  });
  const article = markNoteRuns(h('article', { class: `article al-para${fontCls(it)}` }, minutesCard(it), a.ai ? aimLine(it, o) : null, ...rows, bookEnd(it)));
  article.style.setProperty('--reading-size', `${state.readingSize}px`);
  const pane = h('div', { class: 'scroll book-pane', id: 'reader-scroll', 'data-item': it.id, 'data-pane': `al:${o.key}` }, article);
  pane.addEventListener('scroll', () => { if (!scrollQueued) { scrollQueued = true; requestAnimationFrame(() => { scrollQueued = false; updateReaderChrome(); }); } }, { passive: true });
  return pane;
}
// 并排 with an index: when one pane scrolls, the matched row at its top puts its start boundary at
// the same position in the other pane. One-sided and unresolved rows never drive the other pane;
// no proportional interpolation. Scrolls caused by syncing (or by restoring panes) are ignored.
let bookSyncMute = 0;
function bindSync(a, b) {
  if (!a || !b) return;
  const expect = new Map();
  const rowTop = (pane, el) => rectIn(el).top - rectIn(pane).top;
  const follow = (from, to) => {
    if (performance.now() < bookSyncMute) return;
    if (expect.has(from)) { const e = expect.get(from); expect.delete(from); if (Math.abs(from.scrollTop - e) < 1) return; }
    let cur = null;
    for (const el of from.querySelectorAll('.al-seg')) { if (rowTop(from, el) <= 4) cur = el; else break; }
    if (!cur || cur.dataset.rel !== 'matched') return;
    const twin = to.querySelector(`.al-seg[data-row="${cur.dataset.row}"]`);
    if (!twin) return;
    const delta = Math.max(rowTop(from, cur), -(twin.offsetHeight - 1)); // never past the twin row
    const before = to.scrollTop;
    to.scrollTop = before + rowTop(to, twin) - delta;
    if (Math.abs(to.scrollTop - before) >= 1) expect.set(to, to.scrollTop);
  };
  a.addEventListener('scroll', () => follow(a, b), { passive: true });
  b.addEventListener('scroll', () => follow(b, a), { passive: true });
}

// ---------- 整理本章段落 (reader_repair): one explicit chapter job on the Mac ----------
// The request_id is stored before sending (per item + revision + chapter + expected index version).
// An unknown outcome keeps it and 重试 resends the same id and payload; failed / unknown jobs clear
// it and 再试一次 is a new explicit request with a new id. Nothing is ever resent automatically.
const ACTIVE_REPAIR = new Set(['sending', 'unknown', 'polling', 'reloading']);
const REQUEST_ID = /^[A-Za-z0-9_-]{1,128}$/;
const repairKey = (p) => ['memex.m.alignReq', GATEWAY ? 'gw' : 'demo', p.item_id, p.revision_id, p.chapter, p.expected_alignment_version].join('|');
const freshRequestId = () => `m_${[...crypto.getRandomValues(new Uint8Array(16))].map((x) => x.toString(16).padStart(2, '0')).join('')}`;
const STAGE_TEXT = { queued: tx('排队中'), formatting: tx('正在整理格式'), matching: tx('正在匹配段落'), committing: tx('正在保存'), done: tx('正在保存') };
// A stored id whose outcome was never confirmed (e.g. the page reloaded): offered as 重试, never resent.
function restoreRepair(it, o, a) {
  if (!repairOn() || !a || !['none', 'ready'].includes(a.state)) return;
  if (it.repair && (it.repair.chapter === o.key || ACTIVE_REPAIR.has(it.repair.phase))) return;
  const base = { item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key, expected_alignment_version: a.version };
  const key = repairKey(base); const id = localStorage.getItem(key);
  if (!id || !REQUEST_ID.test(id)) return;
  it.repair = { chapter: o.key, key, phase: 'unknown', error: 'unknown',
    payload: { item_id: base.item_id, revision_id: base.revision_id, chapter: base.chapter, request_id: id, expected_alignment_version: base.expected_alignment_version } };
}
function startRepair(it, chapter) {
  const a = it.book?.align.get(chapter);
  if (!a || !['none', 'ready'].includes(a.state)) { render(); return; }
  const payload = { item_id: it.id, revision_id: it.meta.revision_id, chapter, request_id: freshRequestId(), expected_alignment_version: a.version };
  const rp = { chapter, payload, key: repairKey(payload) };
  try { localStorage.setItem(rp.key, payload.request_id); } catch { toast(tx('无法在这台设备上记下整理请求，没有发送')); return; }
  clearTimeout(it.repair?.timer);
  it.repair = rp;
  sendRepair(it, rp);
}
async function sendRepair(it, rp) {
  Object.assign(rp, { phase: 'sending', error: null });
  softRender();
  try {
    const job = await source.readerAlignRequest({ ...rp.payload });
    noteOutcome(null);
    if (it.repair !== rp) return;
    applyJob(it, rp, job);
  } catch (e) {
    noteOutcome(e);
    if (it.repair !== rp) return;
    const code = e.code || 'unknown';
    // Not answered: the job may exist. Keep the id; 重试 sends the same request again.
    // library_loading is not a definite rejection in reader_v1, so it keeps the id too.
    if (code === 'unknown' || code === 'unavailable' || code === MAC_BUSY) Object.assign(rp, { phase: 'unknown', error: code });
    else {
      localStorage.removeItem(rp.key);
      Object.assign(rp, { phase: 'rejected', error: code });
      if (code === 'stale_chapter_alignment') it.book.align.delete(rp.chapter);
    }
  }
  if (state.item === it) softRender();
}
function applyJob(it, rp, job) {
  rp.job = job;
  if (job.state === 'queued' || job.state === 'running') { rp.phase = 'polling'; pollRepair(it, rp); return; }
  // Outcome unknown: the id stays stored; 查询原任务 sends the same request and gets the original job back.
  if (job.state === 'outcome_unknown') { Object.assign(rp, { phase: 'unknown', error: 'job_unknown' }); return; }
  localStorage.removeItem(rp.key);
  if (job.state === 'succeeded') { rp.phase = 'reloading'; finishRepair(it, rp, job); return; }
  Object.assign(rp, { phase: 'failed', error: job.error_code || 'reader_repair_failed' });
}
const repairShown = (it, rp) => state.item === it && it.repair === rp && route().name === 'item' && !!it.book?.doc && bookChapter(it.book, 'original')?.key === rp.chapter;
// Status every 1.5 s while this chapter is on screen; leaving the chapter or the item stops it.
function pollRepair(it, rp) {
  clearTimeout(rp.timer);
  rp.timer = setTimeout(async () => {
    rp.timer = null;
    if (!repairShown(it, rp)) return;
    try {
      const job = await source.readerAlignStatus({ item_id: it.id, job_id: rp.job.job_id });
      noteOutcome(null); rp.lost = null;
      if (it.repair !== rp) return;
      applyJob(it, rp, job);
    } catch (e) {
      noteOutcome(e);
      if (it.repair !== rp) return;
      if (e.code === 'reader_job_not_found') { localStorage.removeItem(rp.key); Object.assign(rp, { phase: 'failed', error: e.code }); } else { rp.lost = e.code || 'unknown'; pollRepair(it, rp); }
    }
    if (state.item === it) softRender();
  }, 1500);
}
// Success: a changed revision makes every old chunk and index stale, so the item, TOC, chapters and
// index are read again; otherwise only the index. 本章已重新整理 only once the new index is checked.
async function finishRepair(it, rp, job) {
  try {
    if (job.result_revision_id !== it.meta.revision_id) {
      const meta = withRow(await source.itemRead(it.id));
      if (state.item !== it || it.repair !== rp) return;
      const nb = await readBook(meta);
      if (state.item !== it || it.repair !== rp) return;
      if (!nb?.doc) throw codeErr(nb?.error || 'invalid_response');
      const old = it.book;
      if (bookCh(nb, old.pos.side, old.pos.key)) nb.pos = { ...old.pos };
      it.meta = meta; it.book = nb;
    } else it.book.align.delete(rp.chapter);
    const b = it.book; const o = bookCh(b, 'original', rp.chapter); const t = o?.counterpart ? bookCh(b, 'translation', o.counterpart) : null;
    if (!o || !t) throw codeErr('reader_chapter_not_found');
    softRender();
    await alignFor(it, b, o, t).ready;
  } catch (e) {
    noteOutcome(e);
    if (it.repair === rp) Object.assign(rp, { phase: 'failed', error: e.code || 'unknown', committed: true });
  }
  if (state.item === it) softRender();
}
function repairSettled(it, chapter) {
  const rp = it.repair;
  if (!rp || rp.chapter !== chapter || rp.phase !== 'reloading') return;
  const a = it.book?.align.get(chapter);
  if (a?.state === 'ready' || a?.state === 'none') rp.phase = 'done';
  else if (a?.state === 'error') Object.assign(rp, { phase: 'failed', error: a.error, committed: true });
}
// The chapter's 对照 banner: why there is no (complete) pairing, plus the repair action and progress.
function alignBanner(it, o, a, text) {
  const rp = it.repair?.chapter === o.key ? it.repair : null;
  const ask = () => { it.repairAsk = o.key; render(); };
  const act = (label, onclick) => h('button', { class: 'ab-act', onclick }, label);
  const spin = () => h('span', { class: 'spin', 'aria-hidden': 'true' });
  let line = null;
  if (rp) {
    const msg = (code) => h('span', { class: 'grow' }, alignErr(code));
    line = rp.phase === 'sending' ? [spin(), h('span', { class: 'grow' }, tx('正在提交整理请求…'))]
      : rp.phase === 'unknown' ? [h('span', { class: 'grow' }, rp.error === 'job_unknown' ? tx('结果未知：不确定模型请求是否完成，本章没有改动。可以查询原任务，不会自动重试。') : rp.error === 'unavailable' ? tx('无法连接到你的 Mac，整理请求可能没有送达。查询原任务会发送同一个请求。') : tx('没有收到 Mac 的回复，不确定整理是否已经开始。查询原任务会发送同一个请求，不会重复整理。')),
        act(tx('查询原任务'), () => sendRepair(it, rp))]
        : rp.phase === 'polling' ? [spin(), h('span', { class: 'grow' }, STAGE_TEXT[rp.job?.stage] || tx('排队中'), rp.lost ? tx(' · 暂时连不上 Mac，稍后继续查看进度') : '')]
          : rp.phase === 'reloading' ? [spin(), h('span', { class: 'grow' }, tx('正在载入整理结果…'))]
            : rp.phase === 'done' ? [h('span', { class: 'grow' }, tx('本章已重新整理'))]
              : rp.phase === 'failed' && rp.committed ? [h('span', { class: 'grow' }, tx('本章已在 Mac 上整理，但重新载入失败：{0}', alignErr(rp.error))), act(tx('刷新'), () => loadItem(it.id))]
                : rp.error === 'stale_item_revision' ? [msg(rp.error), act(tx('刷新'), () => loadItem(it.id))]
                  : [msg(rp.error), act(tx('再试一次'), ask)];
  }
  const other = !rp && it.repair && ACTIVE_REPAIR.has(it.repair.phase);
  const offer = !rp && text && repairOn() && a && ['none', 'ready'].includes(a.state);
  // Busy reading the index: read just this chapter's index again (chapter texts stay).
  if (!rp && a?.state === 'error' && a.error === MAC_BUSY) line = [h('span', { class: 'grow' }, BUSY_TEXT), act(tx('重试'), () => { it.book.align.delete(o.key); render(); })];
  if (!text && !line) return null;
  return h('div', { class: 'book-note align-banner', role: 'status' },
    text ? h('div', { class: 'ab-row' }, h('span', { class: 'ab-text grow' }, text),
      offer && !other ? act(tx('整理本章段落'), ask) : null) : null,
    offer && other ? h('div', { class: 'ab-row ab-line' }, tx('这本书的另一章正在整理')) : null,
    line ? h('div', { class: `ab-row ab-line${['failed', 'rejected'].includes(rp?.phase) ? ' bad' : ''}`, 'data-phase': rp?.phase || 'busy' }, line) : null);
}
function repairSheet(it) {
  const key = it.repairAsk;
  if (!key || !it.book?.doc) return [];
  const o = bookCh(it.book, 'original', key);
  const close = () => { it.repairAsk = null; render(); };
  return [h('div', { class: 'scrim', onclick: close }),
    h('section', { class: 'sheet repair-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('整理本章段落') },
      h('div', { class: 'grab' }, h('span')), h('h2', {}, tx('整理本章段落')),
      h('div', { class: 'body' },
        h('p', { class: 'repair-ch' }, o?.title || ''),
        h('p', {}, tx('只整理这一章：用 Mac 上的 Low 档位模型修正段落格式，再把原文与译文逐段匹配，最多发出两次请求。')),
        h('p', { class: 'repair-fine' }, tx('不会整理整本书，也不会自动重试。格式有改动时，Mac 会为这本书保存一个新版本。'))),
      h('div', { class: 'sheet-foot' }, h('span', { class: 'state quiet' }),
        h('button', { class: 'btn', onclick: close }, tx('取消')),
        h('button', { class: 'btn primary', onclick: () => { it.repairAsk = null; startRepair(it, key); } }, tx('开始整理'))))];
}
// ---------- 翻译本章 (reader_translation): an AI translation of one English original-only chapter ----------
// Offered only for an original chapter with no counterpart in an English (en / en-*) original
// edition; a chapter that already has a 译文 never gets the action (it would waste tokens). The
// Mac re-checks eligibility in translate_preview (a read: no inference, no write), which also
// returns a completed AI result, read through translate_chunk without any cost. The request is
// sent only after the user confirms the route and budget of a fresh quote. Its request_id and
// quote_id are stored before sending; an unknown outcome keeps them and 查询原任务 sends the same
// payload; failed / unknown jobs clear them and 再试一次 is a new preview + confirmation.
// Nothing here runs for a prefetched chapter, and nothing is resent automatically.
const xlateOn = () => state.caps?.reader_translation === 'available';
// Cross-device discovery of a completed AI translation (translate_metadata: no quote, no credentials)
// and its recorded block correspondence (translate_alignment). Both are reads.
const xlateMetaOn = () => state.caps?.reader_translation_metadata === 'available';
const xlateMapOn = () => state.caps?.reader_translation_alignment === 'available';
const xlateEdit = () => xlateOn() && !!state.caps?.scopes?.includes('edit');
const ACTIVE_XLATE = new Set(['sending', 'unknown', 'polling', 'reading']);
const xlateKey = (p) => ['memex.m.xlateReq', GATEWAY ? 'gw' : 'demo', p.item_id, p.revision_id, p.chapter].join('|');
const XLATE_ERR = {
  // Same wording as the Mac client (memex-client ClientError messages).
  reader_translation_not_needed: tx('这一章已有译文，不需要翻译'),
  reader_translation_language_unsupported: tx('只能翻译英文原文的章节'),
  reader_translation_budget_exceeded: tx('这一章太长，超出了单章翻译预算'),
  reader_translation_quote_stale: tx('确认已过期或设置已变化，请重新确认'),
  reader_translation_unavailable: tx('翻译模型暂不可用，或没有给出可用的结果'),
  no_translation_provider: tx('Mac 上还没有配置翻译模型'),
  stale_reader_translation: tx('AI 译文已变化，请重新载入'),
  invalid_reader_translation_request: tx('翻译请求无效'),
  invalid_translation: tx('AI 译文与校验信息不一致，暂不显示'),
  reader_job_in_progress: tx('这本书正在进行另一个任务，请等它完成后再试'),
  reader_job_not_found: tx('Mac 上找不到这次翻译任务'),
  request_identity_conflict: tx('这次翻译请求与之前的记录不一致，请重新发起'),
  provider_outcome_unknown: tx('不确定模型请求是否完成'),
  stale_item_revision: tx('条目已在 Mac 上更新，请刷新'),
  permission_denied: tx('这次配对没有编辑权限，不能翻译'),
  unknown: tx('没有收到 Mac 的回复'),
};
const xlateErr = (code) => XLATE_ERR[code] || bookErr(code);
// Ineligible reasons worth telling (no action either way); the others say nothing.
const XLATE_TELL = new Set(['reader_translation_budget_exceeded', 'reader_translation_unavailable', 'no_translation_provider']);
// Same format as the Mac: 调用 1 次 · 共 17 tokens · 输入 11 · 输出 3. The total only when every
// dispatch reported one (else 用量未知); input / output only when known; nothing is derived.
function usageText(u) {
  const n = (v) => Number.isSafeInteger(v) && v >= 0;
  if (!u || !n(u.requests_dispatched)) return tx('用量未知');
  if (u.requests_dispatched === 0) return tx('未调用模型');
  const parts = [tx('调用 {0} 次', u.requests_dispatched)];
  parts.push(n(u.reported_total_tokens) && u.unknown_usage_requests === 0 ? tx('共 {0} tokens', u.reported_total_tokens) : tx('用量未知'));
  if (n(u.input_tokens)) parts.push(tx('输入 {0}', u.input_tokens));
  if (n(u.output_tokens)) parts.push(tx('输出 {0}', u.output_tokens));
  return parts.join(' · ');
}
const attribution = (r) => tx('由 {0} 生成，不是出版译本', r?.model_id || '');
// 已完成 x/y 段 when a job stopped part-way (completed batches are never shown as the chapter).
const partialText = (j) => (Number.isSafeInteger(j?.total_blocks) && j.total_blocks > 0 && Number.isSafeInteger(j?.completed_blocks) && j.completed_blocks < j.total_blocks
  ? tx('已完成 {0}/{1} 段，未完成的译文不会显示', j.completed_blocks, j.total_blocks) : null);
// The chapter's AI translation state (eligible | reading | ready | error), started once per shown
// chapter of this book revision. Opening or prefetching a chapter NEVER calls translate_preview
// (it reads credential status and writes a quote): eligibility is decided from the TOC (English
// original, no counterpart) and the Mac re-checks it when the user taps 翻译本章. The only read on
// open is translate_chunk of a projection version this device already saw complete (no quote, no
// credentials); a stale or missing one is forgotten and the chapter simply offers 翻译本章.
const xlateDoneKey = (it, chapter) => ['memex.m.xlateDone', GATEWAY ? 'gw' : 'demo', it.id, it.meta.revision_id, chapter].join('|');
function aiFor(it, b, o) {
  if (!(xlateOn() || xlateMetaOn()) || !o || o.counterpart || !/^en(-|$)/i.test(b.o.language || '')) return null;
  let a = b.ai.get(o.key);
  if (a) return a;
  // With discovery: ask the Mac on every open / reopen / refresh of the chapter (a new book state);
  // a miss is kept only for this open, never as a permanent negative. Nothing is remembered locally.
  // Versions remembered for an older revision of this item are never used again (a title change
  // gives the same result a new view version): forget them; with discovery forget all of them.
  const pre = `${['memex.m.xlateDone', GATEWAY ? 'gw' : 'demo', it.id].join('|')}|`;
  for (const k of Object.keys(localStorage)) if (k.startsWith(pre) && k.endsWith(`|${o.key}`) && (xlateMetaOn() || k !== xlateDoneKey(it, o.key))) localStorage.removeItem(k);
  if (xlateMetaOn()) { a = { state: 'checking' }; b.ai.set(o.key, a); discoverAi(it, b, o, a); return a; }
  a = { state: 'eligible' }; b.ai.set(o.key, a);
  let known = null;
  try { known = JSON.parse(localStorage.getItem(xlateDoneKey(it, o.key)) || 'null'); } catch { /* unreadable: treated as unknown */ }
  if (known && /^[a-f0-9]{64}$/.test(known.version || '') && /^[a-f0-9]{64}$/.test(known.sha256 || '') && Number.isSafeInteger(known.bytes)) {
    (async () => {
      try { await readAi(it, b, o, a, known); noteOutcome(null); } catch (e) {
        if (it.book !== b || e.code === 'cancelled') return;
        if (BOOK_STALE.has(e.code) && bookStale(it)) return;
        // Busy or out of reach (after the bounded retries / reconnects): the version is still good;
        // keep it and offer 重试.
        if (e.code === MAC_BUSY || e.code === 'unavailable') Object.assign(a, { state: 'error', error: e.code });
        else {
          // Gone or replaced on the Mac: forget it; 翻译本章 (preview after the tap) tells the truth.
          localStorage.removeItem(xlateDoneKey(it, o.key));
          Object.assign(a, ['stale_reader_translation', 'invalid_translation', 'invalid_request', 'invalid_reader_translation_request'].includes(e.code) ? { state: 'eligible' } : { state: 'error', error: e.code || 'unknown' });
        }
      }
      restoreXlate(it, o, a);
      if (state.item === it && route().name === 'item') softRender();
    })();
  } else restoreXlate(it, o, a);
  return a;
}
// translate_metadata, then (on a hit) the whole body under its version and, with the mapping read,
// the recorded correspondence. A version race (stale_reader_translation) re-reads all of it
// (bounded); a revision change reads the book again. Errors are shown as errors, never as missing.
async function discoverAi(it, b, o, a, restarts = 0) {
  try {
    const { translation } = await source.readerTranslateMetadata({ item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key });
    if (it.book !== b) return;
    if (!translation) a.state = 'eligible';
    else { await readAi(it, b, o, a, translation); if (xlateMapOn()) await mapAi(it, b, o, a); }
    noteOutcome(null);
  } catch (e) {
    if (it.book !== b || e.code === 'cancelled') return;
    if (BOOK_STALE.has(e.code) && bookStale(it)) return;
    if (e.code === 'stale_reader_translation' && restarts < 2) { for (const k of ['translation', 'text', 'map']) delete a[k]; a.state = 'checking'; discoverAi(it, b, o, a, restarts + 1); return; }
    noteOutcome(e);
    for (const k of ['text', 'map']) delete a[k];
    Object.assign(a, { state: 'error', error: e.code || 'unknown' });
  }
  if (a.state === 'eligible') restoreXlate(it, o, a);
  if (state.item === it && route().name === 'item') softRender();
}
// The recorded block correspondence of AI translation `a` (a.map: loading | ready | missing | error):
// every page under the projection version, an exact partition of the original chapter (reader.chunk
// bytes) and of the AI body (translate_chunk bytes), both SHA-256s checked. Never human offsets.
async function mapAi(it, b, o, a) {
  const tr = a.translation; const m = { state: 'loading' }; a.map = m;
  if (state.item === it) softRender();
  try {
    const pairs = []; let first = null; let offset = 0;
    for (let page = 0; page < 4096; page += 1) {
      const r = await source.readerTranslateAlignment({ item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key, expected_version: tr.version, offset, limit: ALIGN_LIMIT });
      if (it.book !== b || a.map !== m) throw codeErr('cancelled');
      if (first && ['original_sha256', 'translation_sha256', 'original_bytes', 'translation_bytes', 'status', 'total'].some((k) => r[k] !== first[k])) throw codeErr('invalid_alignment');
      first ??= r; pairs.push(...r.pairs);
      if (r.next_offset == null) break;
      offset = r.next_offset;
    }
    if (first.translation_sha256 !== tr.sha256 || first.translation_bytes !== tr.bytes) throw codeErr('invalid_alignment');
    if (first.status === 'missing') { m.state = 'missing'; return; }
    if (pairs.length !== first.total) throw codeErr('invalid_alignment');
    await loadBookChapter(it, 'original', o);
    const eo = b.store.get(`original:${o.key}`);
    if (!eo?.done) throw codeErr(eo?.error || 'cancelled');
    const ob = U8E.encode(eo.text); const tb = U8E.encode(a.text);
    if (first.original_bytes !== ob.length || (globalThis.crypto?.subtle && (await sha256Hex(ob)) !== first.original_sha256)) throw codeErr('invalid_alignment');
    const at = { o: 0, t: 0 }; const bytes = { o: ob, t: tb }; const rows = [];
    try {
      for (const [i, p] of pairs.entries()) {
        const row = { i, kind: p.kind, relation: 'matched', o: null, t: null };
        for (const k of ['o', 't']) {
          const [x, z] = p[k];
          if (x !== at[k] || z <= x || z > bytes[k].length) throw codeErr('invalid_alignment');
          row[k] = U8D.decode(bytes[k].subarray(x, z)); at[k] = z;
          if (k === 't') row.tb = x; // the row's first byte in the complete AI body (译文标记 anchors)
        }
        rows.push(row);
      }
    } catch { throw codeErr('invalid_alignment'); }
    if (at.o !== ob.length || at.t !== tb.length) throw codeErr('invalid_alignment');
    Object.assign(m, { state: 'ready', rows });
  } catch (e) {
    if (e.code === 'stale_reader_translation' || e.code === 'cancelled' || BOOK_STALE.has(e.code)) throw e;
    noteOutcome(e);
    Object.assign(m, { state: 'error', error: e.code || 'unknown' });
  }
}
// Every piece of the completed projection, checked against its byte count and SHA-256.
async function readAi(it, b, o, a, tr) {
  Object.assign(a, { state: 'reading', translation: tr });
  // Without discovery: remembered before reading, so a busy Mac mid-read never loses the published
  // version; a version that later proves gone or invalid is forgotten by aiFor.
  if (!xlateMetaOn()) try { localStorage.setItem(xlateDoneKey(it, o.key), JSON.stringify({ version: tr.version, sha256: tr.sha256, bytes: tr.bytes, route: tr.route })); } catch { /* reopening then offers 翻译本章 */ }
  const parts = []; let offset = 0;
  for (let i = 0; ; i += 1) {
    if (i >= 256) throw codeErr('invalid_translation');
    const r = await source.readerTranslateChunk({ item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key, expected_version: tr.version, offset, limit: BOOK_LIMIT });
    if (it.book !== b) throw codeErr('cancelled');
    if (r.sha256 !== tr.sha256 || r.bytes !== tr.bytes) throw codeErr('invalid_translation');
    parts.push(r.markdown);
    if (r.next_offset == null) break;
    offset = r.next_offset;
  }
  const text = parts.join(''); const bytes = U8E.encode(text);
  if (bytes.length !== tr.bytes || (globalThis.crypto?.subtle && (await sha256Hex(bytes)) !== tr.sha256)) throw codeErr('invalid_translation');
  Object.assign(a, { state: 'ready', text });
}
// A stored request whose outcome was never confirmed (e.g. the page reloaded): offered as
// 查询原任务 with the same payload, never resent by itself. A completed result clears it.
function restoreXlate(it, o, a) {
  const key = xlateKey({ item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key });
  if (a.state === 'ready') { localStorage.removeItem(key); return; }
  if (!xlateEdit() || a.state !== 'eligible' || (it.xlate && (it.xlate.chapter === o.key || ACTIVE_XLATE.has(it.xlate.phase)))) return;
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(key) || 'null'); } catch { /* unreadable: nothing to restore */ }
  if (!saved || !REQUEST_ID.test(saved.request_id || '') || !/^quote_[a-f0-9]{32}$/.test(saved.quote_id || '')) return;
  it.xlate = { chapter: o.key, key, phase: 'unknown', error: 'unknown',
    payload: { item_id: it.id, revision_id: it.meta.revision_id, chapter: o.key, request_id: saved.request_id, quote_id: saved.quote_id, accept_cost: true, expected_translation_version: 'missing' } };
}
// 翻译本章 / 再试一次: a fresh preview, shown in the confirmation sheet; nothing is sent yet.
async function askXlate(it, chapter) {
  const ask = { chapter, state: 'loading' };
  it.xlateAsk = ask; render();
  const b = it.book;
  try {
    const r = await source.readerTranslatePreview({ item_id: it.id, revision_id: it.meta.revision_id, chapter });
    noteOutcome(null);
    if (it.xlateAsk !== ask || it.book !== b) return;
    if (r.translation) {
      // Already translated (e.g. from the Mac): read it, never spend again.
      it.xlateAsk = null;
      const a = { state: 'loading' }; b.ai.set(chapter, a);
      try { const oc = bookCh(b, 'original', chapter); await readAi(it, b, oc, a, r.translation); if (xlateMapOn()) await mapAi(it, b, oc, a); } catch (e) { if (e.code !== 'cancelled') Object.assign(a, { state: 'error', error: e.code || 'unknown' }); }
    } else if (r.eligible && r.quote) Object.assign(ask, { state: 'ready', quote: r.quote });
    else Object.assign(ask, { state: 'error', error: r.reason || 'reader_translation_unavailable' });
  } catch (e) {
    noteOutcome(e);
    if (it.xlateAsk !== ask) return;
    Object.assign(ask, { state: 'error', error: e.code || 'unknown' });
  }
  if (state.item === it) softRender();
}
function startXlate(it, chapter, quote) {
  if (Date.now() > quote.expires_at - 5000) { askXlate(it, chapter); return; } // expired while open: ask again
  it.xlateAsk = null;
  const payload = { item_id: it.id, revision_id: it.meta.revision_id, chapter, request_id: freshRequestId(), quote_id: quote.quote_id, accept_cost: true, expected_translation_version: 'missing' };
  const xp = { chapter, payload, key: xlateKey(payload) };
  try { localStorage.setItem(xp.key, JSON.stringify({ request_id: payload.request_id, quote_id: payload.quote_id })); } catch { toast(tx('无法在这台设备上记下翻译请求，没有发送')); render(); return; }
  clearTimeout(it.xlate?.timer);
  it.xlate = xp;
  sendXlate(it, xp);
}
async function sendXlate(it, xp) {
  Object.assign(xp, { phase: 'sending', error: null });
  softRender();
  try {
    const job = await source.readerTranslateRequest({ ...xp.payload });
    noteOutcome(null);
    if (it.xlate !== xp) return;
    applyXlate(it, xp, job);
  } catch (e) {
    noteOutcome(e);
    if (it.xlate !== xp) return;
    const code = e.code || 'unknown';
    // Not answered: the job may exist. Keep the id; 查询原任务 sends the same request again.
    // library_loading is not a definite rejection in reader_v1, so it keeps the id too (the Mac
    // gateway already reports it as unknown for a dispatched write).
    if (code === 'unknown' || code === 'unavailable' || code === MAC_BUSY) Object.assign(xp, { phase: 'unknown', error: code });
    else {
      localStorage.removeItem(xp.key);
      Object.assign(xp, { phase: 'rejected', error: code });
      if (code === 'reader_translation_not_needed') it.book?.ai.delete(xp.chapter);
    }
  }
  if (state.item === it) softRender();
}
function applyXlate(it, xp, job) {
  xp.job = job;
  if (job.state === 'queued' || job.state === 'running') { xp.phase = 'polling'; pollXlate(it, xp); return; }
  // Outcome unknown: the id stays stored; 查询原任务 sends the same request and gets this job back.
  if (job.state === 'outcome_unknown') { Object.assign(xp, { phase: 'unknown', error: 'job_unknown' }); return; }
  localStorage.removeItem(xp.key);
  if (job.state === 'succeeded') { xp.phase = 'reading'; finishXlate(it, xp, job); return; }
  Object.assign(xp, { phase: 'failed', error: job.error_code || 'reader_translation_unavailable' });
}
const xlateShown = (it, xp) => state.item === it && it.xlate === xp && route().name === 'item' && !!it.book?.doc && bookChapter(it.book, 'original')?.key === xp.chapter;
// Status every 1.5 s while this chapter is on screen; leaving the chapter or the item stops it.
function pollXlate(it, xp) {
  clearTimeout(xp.timer);
  xp.timer = setTimeout(async () => {
    xp.timer = null;
    if (!xlateShown(it, xp)) return;
    try {
      const job = await source.readerTranslateStatus({ item_id: it.id, job_id: xp.job.job_id });
      noteOutcome(null); xp.lost = null;
      if (it.xlate !== xp) return;
      applyXlate(it, xp, job);
    } catch (e) {
      noteOutcome(e);
      if (it.xlate !== xp) return;
      if (e.code === 'reader_job_not_found') { localStorage.removeItem(xp.key); Object.assign(xp, { phase: 'failed', error: e.code }); } else { xp.lost = e.code || 'unknown'; pollXlate(it, xp); }
    }
    if (state.item === it) softRender();
  }, 1500);
}
// Success: 本章 AI 译文已就绪 only once the whole projection is read and its hash checked.
async function finishXlate(it, xp, job) {
  const b = it.book; const o = bookCh(b, 'original', xp.chapter);
  const a = { state: 'loading' }; b.ai.set(xp.chapter, a);
  try {
    if (!o) throw codeErr('reader_chapter_not_found');
    await readAi(it, b, o, a, job.translation);
    if (xlateMapOn()) await mapAi(it, b, o, a);
    if (it.xlate === xp) xp.phase = 'done';
  } catch (e) {
    if (e.code === 'cancelled') return;
    noteOutcome(e);
    Object.assign(a, { state: 'error', error: e.code || 'unknown' });
    if (it.xlate === xp) Object.assign(xp, { phase: 'failed', error: e.code || 'unknown', committed: true });
  }
  if (state.item === it) softRender();
}
// The banner above an original-only chapter: the 翻译本章 action, or the job's progress and outcome.
// Wording and order follow the Mac client (outcome first, then reason · progress · usage).
function xlateBanner(it, o, a, mode) {
  const xp = it.xlate?.chapter === o.key ? it.xlate : null;
  const act = (label, onclick) => h('button', { class: 'ab-act', onclick }, label);
  const spin = () => h('span', { class: 'spin', 'aria-hidden': 'true' });
  const grow = (...kids) => h('span', { class: 'grow' }, ...kids);
  const toAi = mode === 'original' ? act(tx('查看 AI 译文'), () => { if (!editGuard()) return; state.lang = 'trans'; render(); }) : null;
  let text = null; let line = null; let details = null;
  if (xp) {
    const j = xp.job;
    const counted = Number.isSafeInteger(j?.total_blocks) && j.total_blocks > 0 && Number.isSafeInteger(j?.completed_blocks) ? ` · ${tx('{0}/{1} 段', j.completed_blocks, j.total_blocks)}` : '';
    const usage = j ? usageText(j.usage) : null;
    line = xp.phase === 'sending' ? [spin(), grow(tx('正在提交翻译请求…'))]
      : xp.phase === 'unknown' ? [grow(tx('结果未知：不确定翻译请求是否完成，没有显示不完整的译文。可以查询原任务，不会自动重试。')), act(tx('查询原任务'), () => sendXlate(it, xp))]
        : xp.phase === 'polling' ? [spin(), grow(tx('正在翻译本章…'), counted, xp.lost ? tx(' · 暂时连不上 Mac，稍后继续查看进度') : '')]
          : xp.phase === 'reading' ? [spin(), grow(tx('正在载入 AI 译文…'))]
            : xp.phase === 'done' ? [grow(tx('本章 AI 译文已就绪')), toAi]
              : xp.phase === 'failed' && xp.committed && xp.error === MAC_BUSY ? [grow(BUSY_TEXT), act(tx('重试'), () => { it.xlate = null; it.book.ai.delete(o.key); render(); })]
              : xp.phase === 'failed' && xp.committed ? [grow(tx('AI 译文已生成，但没有读取成功：{0}', xlateErr(xp.error))), act(tx('重新读取'), () => loadItem(it.id))]
                : xp.phase === 'failed' ? [grow(tx('翻译没有成功')), act(tx('再试一次'), () => askXlate(it, o.key))]
                  : xp.error === 'stale_item_revision' ? [grow(xlateErr(xp.error)), act(tx('刷新'), () => loadItem(it.id))]
                    : [grow(xlateErr(xp.error)), xp.error === 'reader_translation_not_needed' ? null : act(tx('再试一次'), () => askXlate(it, o.key))];
    const parts = xp.phase === 'failed' && !xp.committed ? [xlateErr(xp.error), partialText(j), usage]
      : xp.phase === 'unknown' ? [partialText(j), usage] : xp.phase === 'done' ? [usage] : [];
    details = parts.filter(Boolean).join(' · ') || null;
  } else if (a?.state === 'eligible' && xlateEdit()) {
    const other = it.xlate && ACTIVE_XLATE.has(it.xlate.phase);
    text = [grow(tx('这一章只有英文原文')), other ? null : act(tx('翻译本章'), () => askXlate(it, o.key))];
    if (other) line = [grow(tx('这本书的另一章正在翻译'))];
  } else if (a?.state === 'ready' && mode === 'original') text = [h('span', { class: 'ai-trans-badge' }, tx('AI 译文')), grow(tx('这一章有 AI 译文')), toAi];
  else if (a?.state === 'none' && XLATE_TELL.has(a.reason)) text = [grow(xlateErr(a.reason))];
  else if (a?.state === 'error' && a.error === MAC_BUSY) text = [grow(BUSY_TEXT), act(tx('重试'), () => { it.book.ai.delete(o.key); render(); })];
  else if (a?.state === 'error') text = [grow(tx('AI 译文已生成，但没有读取成功：{0}', xlateErr(a.error))), act(tx('重新读取'), () => { it.book.ai.delete(o.key); render(); })];
  if (!text && !line) return null;
  return h('div', { class: 'book-note align-banner xlate-banner', role: 'status', 'data-chapter': o.key },
    text ? h('div', { class: 'ab-row ab-text' }, text) : null,
    line ? h('div', { class: `ab-row ab-line${['failed', 'rejected'].includes(xp?.phase) ? ' bad' : ''}`, 'data-phase': xp?.phase || 'other' }, line) : null,
    details ? h('div', { class: 'ab-row ab-usage' }, details) : null);
}
// The 「AI 译文」 caption: badge + 由 {model} 生成，不是出版译本 (never credited to the human translator).
const aiCaption = (a, cls) => h('div', { class: cls }, h('span', { class: 'ai-trans-badge' }, tx('AI 译文')), h('span', { class: 'ai-trans-src' }, attribution(a.translation?.route)));
function aiPane(it, o, a, { primary = false, lead = false, end = primary, rows = null } = {}) {
  // With the recorded correspondence the AI body is drawn row by row (same bytes) for synced 并排.
  const text = rows ? h('div', { class: 'book-text ai-text', lang: 'zh-CN' }, rows.map((r) => markNoteRuns(h('div', { class: `al-seg${r.kind === 'note' ? ' al-note lnote' : ''}`, 'data-row': String(r.i), 'data-rel': 'matched' }, aiBlocks(it, o, a, r.t, r.tb)))))
    : h('div', { class: 'book-text ai-text', lang: 'zh-CN' }, aiBlocks(it, o, a, a.text, 0));
  const article = h('article', { class: `article${fontCls(it)}` }, lead ? aiCaption(a, 'ai-trans-head') : null, aimLine(it, o),
    markNoteRuns(text), end ? bookEnd(it) : null);
  article.style.setProperty('--reading-size', `${state.readingSize}px`);
  const pane = h('div', { class: `scroll book-pane ai-pane${primary && vertNow(it) ? ' vert' : ''}`, id: primary ? 'reader-scroll' : null, 'data-item': it.id, 'data-pane': `ai:${o.key}` }, article);
  if (pane.classList.contains('vert')) bindVert(pane);
  if (primary) pane.addEventListener('scroll', () => { if (!scrollQueued) { scrollQueued = true; requestAnimationFrame(() => { scrollQueued = false; updateReaderChrome(); }); } }, { passive: true });
  return pane;
}
function xlateSheet(it) {
  const ask = it.xlateAsk;
  if (!ask || !it.book?.doc) return [];
  const o = bookCh(it.book, 'original', ask.chapter); const q = ask.quote;
  const close = () => { it.xlateAsk = null; render(); };
  const row = (k, v) => h('div', { class: 'xl-row' }, h('dt', {}, k), h('dd', {}, v));
  return [h('div', { class: 'scrim', onclick: close }),
    h('section', { class: 'sheet repair-sheet xlate-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('翻译本章前请确认') },
      h('div', { class: 'grab' }, h('span')), h('h2', {}, tx('翻译本章前请确认')),
      h('div', { class: 'body' },
        h('p', { class: 'repair-ch' }, o?.title || ''),
        h('p', {}, tx('只翻译这一章，结果单独保存为 AI 译文，不改动原书和人工译文。')),
        ask.state === 'loading' ? h('p', { class: 'repair-fine' }, tx('正在读取翻译设置…'))
          : ask.state === 'error' ? h('p', { class: 'xl-bad' }, xlateErr(ask.error))
            : h('dl', { class: 'xl-quote' },
              row(tx('模型'), q.route.model_id),
              row(tx('推理强度'), q.route.reasoning_effort || tx('默认')),
              row(tx('服务'), q.route.provider_id),
              row(tx('计划请求'), tx('{0} 次（上限）', q.planned_requests)),
              row(tx('单次上限'), tx('输入 {0} 字节 · 输出 {1} tokens', q.budget.max_input_bytes_per_request, q.budget.max_completion_tokens_per_request)),
              row(tx('合计上限'), tx('输入 {0} 字节 · 输出 {1} tokens', q.budget.max_total_input_bytes, q.budget.max_total_completion_tokens)),
              q.estimated_cost ? row(tx('估计费用'), tx('预估费用约 {0} {1}', q.estimated_cost.amount, q.estimated_cost.currency)) : null),
        h('p', { class: 'repair-fine' }, tx('预算是上限，不是预计用量。失败或结果未知时不会自动重试。'))),
      h('div', { class: 'sheet-foot' }, h('span', { class: 'state quiet' }),
        h('button', { class: 'btn', onclick: close }, tx('取消')),
        h('button', { class: 'btn primary', disabled: ask.state !== 'ready', onclick: () => startXlate(it, ask.chapter, q) }, tx('确认翻译'))))];
}
// ---------- 编辑 Markdown (reader_edit): an owner edit of one chapter of one side ----------
// Same behaviour and wording as the Mac reader (memex-reader editing.rs, memex-client book.rs).
// Only in 原文 or 译文 with that side's chapter loaded; needs reader_edit and this pairing's edit
// grant. The draft holds that chapter's exact text (never the whole book). Saving is staged:
// edit_begin → edit_chunk (≤8 KiB, whole code points) from what the Mac already received →
// edit_commit, bound to the revision and the chapter's SHA-256. The request id is stored before
// anything is sent; a lost or malformed reply leaves the outcome unknown: draft and request stay,
// and only 核实保存结果 replays the SAME request (resuming the upload or returning the original
// receipt), never a second revision. Nothing is resent by itself, also after the session ends.
const editOn = () => state.caps?.reader_edit === 'available' && !!state.caps?.scopes?.includes('edit');
const revTextOn = () => state.caps?.reader_revision_text === 'available';
const EDIT_MAX = 16 * 1024 * 1024;
const EDIT_CHUNK = 8192;
// Only {request_id, sha256, bytes, base hash} are stored on the phone, never the chapter text.
const editKey = (p) => ['memex.m.editReq', GATEWAY ? 'gw' : 'demo', p.item_id, p.revision_id, p.side, p.chapter].join('|');
const EDIT_ERR = {
  invalid_reader_edit: tx('这一章的内容不能保存：不能为空、只有空白或含有不允许的字符'),
  reader_edit_too_large: tx('这一章超过 16 MiB，不能保存'),
  reader_edit_not_found: tx('找不到这次上传，请重新保存'),
  reader_edit_upload_conflict: tx('这次上传与先前的不一致，未保存。请重新保存'),
  request_id_conflict: tx('这次保存与先前的请求不一致，未写入。请重新载入后再保存'),
  item_revision_limit_exceeded: tx('该条目的修订次数已达上限'),
  permission_denied: tx('这次配对没有编辑权限，不能保存'),
  invalid_request: tx('这台 Mac 不接受编辑本章的请求'),
  unknown: tx('没有收到 Mac 的回复'),
  session_revoked: tx('这次配对已失效'), session_expired: tx('这次配对已过期'),
  revision_text_unavailable: tx('Mac 上读不到这个版本的文字'),
  invalid_revision_text: tx('读到的版本文字不完整或与记录不符，没有显示对比'),
};
const editErr = (code) => EDIT_ERR[code] || bookErr(code);
// Rejections that prove this request wrote nothing; anything else leaves the outcome unknown.
const EDIT_DEFINITIVE = new Set(['invalid_reader_edit', 'reader_edit_too_large', 'reader_edit_not_found', 'reader_edit_upload_conflict', 'request_id_conflict',
  'library_resource_not_found', 'item_revision_limit_exceeded', 'permission_denied', 'reader_chapter_not_found', 'reader_side_not_found', 'stale_book_manifest', 'invalid_request']);
// After an earlier unknown attempt these only say the session cannot check now, not that nothing was saved.
const EDIT_SESSION = new Set(['permission_denied', 'session_revoked', 'session_expired']);
const editDirty = (e) => !!e && e.draft !== e.base;
const editHeld = (e) => !!e && (e.phase === 'saving' || !!e.pending);
const sideName = (side) => (side === 'original' ? tx('原文') : tx('译文'));
// The chapter on screen when one side of it can be edited: 原文 or 译文, that side has it, text loaded.
function editTarget(it) {
  const b = it.book;
  if (!b?.doc || state.lang === 'bi' || state.insView !== 'text') return null;
  const side = state.lang === 'trans' ? 'translation' : 'original';
  const ch = bookChapter(b, side); const e = ch && b.store.get(`${side}:${ch.key}`);
  if (!ch || !e?.done) return null;
  return { side, key: ch.key, title: ch.title.trim(), text: e.text };
}
function editHint(it) {
  if (state.lang === 'bi') return tx('切到原文或译文后编辑本章');
  return bookChapter(it.book, state.lang === 'trans' ? 'translation' : 'original') ? tx('正在载入本章…') : tx('这一侧没有这一章，不能编辑');
}
// Navigation guard: a dirty draft asks before it is dropped; a save in flight or with an unknown
// outcome blocks until it is checked (its request must stay answerable).
function editGuard() {
  const it = state.item; const e = it?.edit;
  if (!e) return true;
  if (editHeld(e)) { e.open = true; render(); toast(e.phase === 'saving' ? tx('正在保存这一章，请稍候') : tx('保存结果还未核实：请先点「核实保存结果」')); return false; }
  if (!editDirty(e)) { it.edit = null; return true; }
  if (!confirm(tx('本章有未保存的草稿，离开会丢失草稿。确定离开吗？'))) { e.open = true; render(); return false; }
  it.edit = null;
  return true;
}
addEventListener('beforeunload', (ev) => { const e = state.item?.edit; if (editHeld(e) || editDirty(e)) { ev.preventDefault(); ev.returnValue = ''; } });
async function openEditor(it) {
  const cur = it.edit; const t = editTarget(it);
  // The same chapter (or a draft that must not be dropped) reopens as it is.
  if (cur && (editHeld(cur) || editDirty(cur) || !t || (cur.side === t.side && cur.key === t.key && cur.revision === it.meta.revision_id))) { cur.open = true; render(); return; }
  if (!t) { toast(editHint(it)); return; }
  if (!globalThis.crypto?.subtle) { toast(tx('这个浏览器无法校验章节内容，不能编辑')); return; }
  const baseSha = await sha256Hex(U8E.encode(t.text));
  if (state.item !== it || it.book?.doc == null) return;
  const e = { side: t.side, key: t.key, title: t.title, revision: it.meta.revision_id, base: t.text, baseSha, draft: t.text, open: true, phase: 'editing', pending: null, msg: null };
  // A stored request whose outcome was never confirmed (e.g. the page reloaded): offered as 核实保存结果.
  const ids = { item_id: it.id, revision_id: e.revision, side: e.side, chapter: e.key };
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(editKey(ids)) || 'null'); } catch { /* unreadable: nothing to restore */ }
  if (saved && REQUEST_ID.test(saved.request_id || '') && /^[a-f0-9]{64}$/.test(saved.sha256 || '') && Number.isSafeInteger(saved.bytes) && saved.expected_chapter_sha256 === baseSha) {
    e.pending = { ...ids, expected_chapter_sha256: baseSha, request_id: saved.request_id, bytes: saved.bytes, sha256: saved.sha256, markdown: null };
    Object.assign(e, { phase: 'unknown', why: 'unknown' });
  }
  it.edit = e;
  render();
}
// begin → remaining chunks → commit; every reply is checked against the request (a mismatch is unknown).
async function uploadChapter(p, onProgress) {
  const check = (r) => {
    const moved = r.revision_id !== p.revision_id;
    if (r.item_id !== p.item_id || r.request_id !== p.request_id || r.side !== p.side || r.chapter !== p.chapter || r.chapter_sha256 !== p.sha256 || r.changed !== moved || !r.revision_id) throw codeErr('unknown');
    return r;
  };
  const up = await source.readerEditBegin({ item_id: p.item_id, revision_id: p.revision_id, side: p.side, chapter: p.chapter, expected_chapter_sha256: p.expected_chapter_sha256, request_id: p.request_id, bytes: p.bytes, sha256: p.sha256 });
  if (up.state === 'committed') { if (up.received_bytes !== p.bytes || !up.receipt) throw codeErr('unknown'); return check(up.receipt); }
  if (up.receipt) throw codeErr('unknown');
  // Restored after a reload: the text is not on this phone, so an unfinished upload cannot go on.
  if (p.markdown == null) throw codeErr('edit_draft_gone');
  const bytes = U8E.encode(p.markdown);
  if (bytes.length !== p.bytes || up.received_bytes > bytes.length || (up.received_bytes < bytes.length && (bytes[up.received_bytes] & 0xc0) === 0x80)) throw codeErr('unknown');
  const max = Math.min(up.max_chunk_bytes, EDIT_CHUNK);
  let at = up.received_bytes;
  onProgress(at, bytes.length);
  while (at < bytes.length) {
    let end = Math.min(at + max, bytes.length);
    while (end > at && end < bytes.length && (bytes[end] & 0xc0) === 0x80) end -= 1;
    if (end === at) throw codeErr('unknown');
    const r = await source.readerEditChunk({ item_id: p.item_id, edit_id: up.edit_id, offset: at, markdown: U8D.decode(bytes.subarray(at, end)) });
    if (r.edit_id !== up.edit_id || r.received_bytes !== end) throw codeErr('unknown');
    at = end;
    onProgress(at, bytes.length);
  }
  return check(await source.readerEditCommit({ item_id: p.item_id, edit_id: up.edit_id }));
}
async function saveChapter(it) {
  const e = it.edit;
  if (!e || e.phase === 'saving' || e.phase === 'stale' || !editOn()) return;
  const replay = !!e.pending;
  let p = e.pending;
  if (!p) {
    if (!editDirty(e)) return;
    const bytes = U8E.encode(e.draft);
    // As the Markdown editor: refused here, nothing is sent.
    if (bytes.length > EDIT_MAX) { e.msg = { bad: true, text: editErr('reader_edit_too_large') }; render(); return; }
    if (!e.draft.trim() || e.draft.includes('\0')) { e.msg = { bad: true, text: editErr('invalid_reader_edit') }; render(); return; }
    p = { item_id: it.id, revision_id: e.revision, side: e.side, chapter: e.key, expected_chapter_sha256: e.baseSha, request_id: freshRequestId(), bytes: bytes.length, sha256: await sha256Hex(bytes), markdown: e.draft };
    try { localStorage.setItem(editKey(p), JSON.stringify({ request_id: p.request_id, sha256: p.sha256, bytes: p.bytes, expected_chapter_sha256: p.expected_chapter_sha256 })); } catch {
      toast(tx('无法在这台设备上记下保存请求，没有发送')); return;
    }
    e.pending = p;
  }
  const key = editKey(p);
  Object.assign(e, { phase: 'saving', msg: null, sent: 0, total: p.bytes });
  it.editDone = null;
  render();
  const progress = (at, total) => {
    e.sent = at; e.total = total;
    const el = document.getElementById('ce-progress');
    if (el) el.textContent = tx('正在上传 {0} / {1} KiB', (at / 1024).toFixed(1), (total / 1024).toFixed(1));
  };
  let receipt;
  try {
    receipt = await uploadChapter(p, progress);
    noteOutcome(null);
  } catch (err) {
    noteOutcome(err);
    if (it.edit !== e) return;
    const code = err.code || 'unknown';
    if (code === 'stale_item_revision') {
      localStorage.removeItem(key);
      Object.assign(e, { pending: null, phase: 'stale', msg: { bad: true, text: tx('这一章已在别处更新，没有保存。草稿还在：可以先复制草稿，再重新载入本章。') } });
    } else if (code === 'reader_job_in_progress') {
      // Nothing written; the same request is checked again once the job ends.
      Object.assign(e, { phase: 'editing', msg: { bad: true, text: tx('这本书有一章正在整理，暂时不能保存。草稿保留，整理结束后再保存。') } });
    } else if (code === 'edit_draft_gone') {
      localStorage.removeItem(key);
      Object.assign(e, { pending: null, phase: 'editing', msg: { bad: true, text: tx('上次的保存没有完成，没有建立新版本。那份草稿没有留在这台设备上，可以重新编辑后保存。') } });
    } else if (EDIT_DEFINITIVE.has(code) && !(replay && EDIT_SESSION.has(code))) {
      localStorage.removeItem(key);
      Object.assign(e, { pending: null, phase: 'editing', msg: { bad: true, text: editErr(code) } });
    } else Object.assign(e, { phase: 'unknown', why: code });
    if (state.item === it) render();
    return;
  }
  localStorage.removeItem(key);
  if (it.edit !== e) return;
  if (!receipt.changed) {
    // Identical body: no new revision; the draft now matches what is saved.
    if (p.markdown != null) Object.assign(e, { base: p.markdown, draft: p.markdown });
    Object.assign(e, { baseSha: p.sha256, pending: null, phase: 'editing', msg: { bad: false, text: tx('没有改动：这一章与当前版本相同，没有新建版本。') } });
    if (state.item === it) render();
    return;
  }
  // A new revision: read the book again at it and come back to this chapter (alignment of the
  // edited chapter now reads as missing). The other side, other chapters and every other draft stay.
  it.edit = null;
  const done = { side: p.side, chapter: p.chapter, title: e.title, from: p.revision_id, to: receipt.revision_id, fromSha: p.expected_chapter_sha256, toSha: receipt.chapter_sha256, phase: 'reloading' };
  it.editDone = done;
  if (state.item === it) render();
  try {
    await reloadBook(it);
    const ch = bookCh(it.book, done.side, done.chapter);
    if (!ch) throw codeErr('reader_chapter_not_found');
    if (it.meta.revision_id === done.to) {
      await loadBookChapter(it, done.side, ch);
      const ent = it.book.store.get(`${done.side}:${ch.key}`);
      if (!ent?.done) throw codeErr(ent?.error || 'invalid_response');
      if ((await sha256Hex(U8E.encode(ent.text))) !== done.toSha) throw codeErr('invalid_response');
    }
    done.phase = 'done';
  } catch (err) {
    if (err.code === 'cancelled') return;
    noteOutcome(err);
    Object.assign(done, { phase: 'failed', error: err.code || 'unknown' });
  }
  if (state.item === it) softRender();
}
// Reads the item and its book again (a new revision), keeping the chapter, mode and other state.
async function reloadBook(it) {
  const meta = withRow(await source.itemRead(it.id));
  if (state.item !== it) throw codeErr('cancelled');
  const nb = await readBook(meta);
  if (state.item !== it) throw codeErr('cancelled');
  if (!nb?.doc) throw codeErr(nb?.error || 'invalid_response');
  const old = it.book;
  if (old?.pos && bookCh(nb, old.pos.side, old.pos.key)) nb.pos = { ...old.pos };
  it.meta = meta; it.book = nb;
}
async function reloadChapter(it) {
  const e = it.edit;
  if (!e || editHeld(e)) return;
  it.edit = null;
  render();
  try { await reloadBook(it); noteOutcome(null); } catch (err) { if (err.code !== 'cancelled') { noteOutcome(err); toast(bookErr(err.code || 'unknown')); } }
  if (state.item === it) render();
}
async function copyDraft(e) {
  try { await navigator.clipboard.writeText(e.draft); toast(tx('已复制草稿')); } catch {
    const ta = document.querySelector('.ce-text');
    let ok = false;
    if (ta) { ta.focus(); ta.select(); try { ok = document.execCommand('copy'); } catch { ok = false; } }
    toast(ok ? tx('已复制草稿') : tx('无法自动复制，已选中草稿，请手动复制'));
  }
}
function discardEdit(it) {
  const e = it.edit;
  if (!e || editHeld(e)) return;
  if (editDirty(e) && !confirm(tx('放弃这一章的修改吗？'))) return;
  it.edit = null; render();
}
// The banner above the chapter: a folded draft, or the outcome of the last save of this chapter.
function editBanner(it) {
  const b = it.book; const e = it.edit; const d = it.editDone;
  const act = (label, onclick) => h('button', { class: 'ab-act', onclick }, label);
  const shown = (side, key) => bookShown(b).some(([s, c]) => s === side && c.key === key);
  if (e && !e.open && shown(e.side, e.key)) {
    const text = e.phase === 'unknown' ? tx('这一章的保存结果未知：可能已经保存，也可能没有。') : e.phase === 'stale' ? tx('这一章已在别处更新，草稿没有保存。')
      : editDirty(e) ? tx('这一章有未保存的草稿') : tx('正在编辑这一章');
    return h('div', { class: 'book-note align-banner edit-banner', role: 'status', 'data-phase': e.phase },
      h('div', { class: 'ab-row' }, h('span', { class: 'grow' }, text), act(tx('继续编辑'), () => { e.open = true; render(); })));
  }
  if (!d || !shown(d.side, d.chapter)) return null;
  const line = d.phase === 'reloading' ? [h('span', { class: 'spin', 'aria-hidden': 'true' }), h('span', { class: 'grow' }, tx('正在载入保存后的这一章…'))]
    : d.phase === 'failed' ? [h('span', { class: 'grow' }, tx('已保存这一章，但重新载入失败：{0}', bookErr(d.error))), act(tx('刷新'), () => loadItem(it.id))]
      : [h('span', { class: 'grow' }, tx('已保存这一章，新版本已建立，旧版本保留。改过的这一章需要重新整理段落对照。')),
        revTextOn() ? act(tx('查看本次修改'), () => openDiff(it, d)) : null];
  return h('div', { class: 'book-note align-banner edit-banner', role: 'status', 'data-phase': `saved-${d.phase}` }, h('div', { class: 'ab-row' }, line));
}
// The editor: a full-screen sheet over the reader, holding exactly the chapter of one side.
function editSheet(it) {
  const e = it.edit;
  if (!e?.open || !it.book?.doc) return [];
  const dirty = editDirty(e);
  const held = editHeld(e);
  const ta = h('textarea', { class: 'ce-text', 'aria-label': tx('本章 Markdown'), spellcheck: 'false', autocapitalize: 'off', autocomplete: 'off', autocorrect: 'off',
    lang: bookEd(it.book, e.side).language || null, readonly: held || e.phase === 'stale' ? true : null });
  ta.value = e.draft;
  const stateEl = h('span', { class: `state${dirty ? '' : ' quiet'}` });
  const saveLabel = e.phase === 'saving' ? tx('保存中…') : e.pending ? tx('核实保存结果') : tx('保存');
  const save = h('button', { class: 'btn primary', 'data-act': 'save', onclick: () => saveChapter(it) }, saveLabel);
  const discard = held ? null : h('button', { class: 'btn', onclick: () => discardEdit(it) }, tx('放弃修改'));
  const paint = () => {
    const d = editDirty(e);
    if (discard) discard.disabled = !d;
    stateEl.textContent = e.pending ? '' : d ? tx('未保存') : tx('已保存');
    stateEl.className = `state${d ? '' : ' quiet'}`;
    // After a conflict the same base can only fail again: copy, then reload.
    save.disabled = e.phase === 'saving' || e.phase === 'stale' || (!d && !e.pending) || !editOn();
  };
  ta.addEventListener('input', () => { e.draft = ta.value; if (e.msg && !e.msg.bad) e.msg = null; paint(); });
  paint();
  const msg = e.phase === 'saving' ? h('div', { class: 'ce-msg' }, h('span', { class: 'spin', 'aria-hidden': 'true' }), h('span', { id: 'ce-progress', class: 'grow' }, tx('正在上传 {0} / {1} KiB', (e.sent / 1024).toFixed(1), (e.total / 1024).toFixed(1))))
    : e.phase === 'unknown' ? h('div', { class: 'ce-msg bad', 'data-phase': 'unknown' }, h('span', { class: 'grow' },
      tx('保存结果未知（{0}）：可能已经保存，也可能没有。草稿和这次请求都保留；点「核实保存结果」会用同一请求核对，不会重复建立版本。', editErr(e.why || 'unknown'))))
      : e.msg ? h('div', { class: `ce-msg${e.msg.bad ? ' bad' : ''}`, 'data-phase': e.phase }, h('span', { class: 'grow' }, e.msg.text)) : null;
  const close = () => { e.open = false; render(); };
  const foot = e.phase === 'stale'
    ? [h('button', { class: 'btn', onclick: () => copyDraft(e) }, tx('复制草稿')), h('button', { class: 'btn primary', onclick: () => reloadChapter(it) }, tx('放弃草稿并重新载入'))]
    : [stateEl, discard, save];
  return [h('section', { class: 'sheet ce-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('编辑 Markdown'), 'data-side': e.side, 'data-chapter': e.key },
    ribbon(), h('header', { class: 'ce-head' },
      h('button', { class: 'icon-btn', 'aria-label': tx('收起编辑'), onclick: close }, icon('down', 20)),
      h('div', { class: 'ce-title' }, h('div', { class: 'ce-what' }, tx('正在编辑：{0} · {1}', sideName(e.side), e.title)),
        h('div', { class: 'ce-sub' }, tx('只保存这一章的这一侧；保存后建立新版本，旧版本保留')))),
    msg, ta, h('div', { class: 'sheet-foot ce-foot' }, foot))];
}

// ---------- 对比修改 (reader_revision_text): line and in-line differences of two revisions ----------
// Both complete texts are read by bounded chunks (≤8 KiB, bound to the first chunk's SHA-256) and
// checked (length, hash, and the hash recorded for that version) before anything is drawn; a
// partial text is never diffed. The diff runs here, without any model: deletions red, insertions
// green, a changed line as its deletion + insertion with the changed words marked inside.
const DIFF_LIMIT = 8192;
const DIFF_CONTEXT = 2;
async function readRevisionText(it, revision_id, target, expectSha, onBytes) {
  const parts = []; let start = 0; let sha = 'any'; let total = null;
  for (let i = 0; ; i += 1) {
    if (i > 4200) throw codeErr('invalid_revision_text');
    const r = await source.readerRevisionChunk({ item_id: it.id, revision_id, target, start_byte: start, limit_bytes: DIFF_LIMIT, expected_sha256: sha });
    if ((sha !== 'any' && r.sha256 !== sha) || (total !== null && r.total_bytes !== total) || r.start_byte !== start) throw codeErr('invalid_revision_text');
    sha = r.sha256; total = r.total_bytes;
    parts.push(r.text);
    start = r.end_byte;
    onBytes(start, total);
    if (r.next_offset == null) break;
    start = r.next_offset;
  }
  const text = parts.join(''); const bytes = U8E.encode(text);
  if (bytes.length !== total || (await sha256Hex(bytes)) !== sha || (expectSha && sha !== expectSha)) throw codeErr('invalid_revision_text');
  return text;
}
// `d.target` is {kind:'body'} or a chapter of one side (default: d.side / d.chapter). With `d.fmt`
// (a 格式整理 target handle) the proof list of format_changes is shown too; its source/result
// hashes then bind both texts.
async function openDiff(it, d) {
  if (!revTextOn() && !d.fmt) return;
  const target = d.target || { kind: 'chapter', side: d.side, chapter: d.chapter };
  const df = { title: d.title, side: target.side || null, chapter: target.chapter || null, target, from: d.from, to: d.to, fromSha: d.fromSha, toSha: d.toSha,
    fmt: d.fmt || null, state: 'loading', got: 0, total: null, open: new Set() };
  it.diff = df; render();
  const paint = () => { if (it.diff === df && (state.item === it || it.host)) softRender(); };
  if (df.fmt) {
    df.rec = { state: 'loading', rows: [], next: 0, texts: new Map(), open: !revTextOn() };
    try {
      const page = await fmtChangesPage(df.fmt, 0);
      if (it.diff !== df) return;
      Object.assign(df.rec, { state: 'ready', rows: page.changes, total: page.total, next: page.next_offset });
      df.fromSha = page.source_sha256; df.toSha = page.result_sha256;
      noteOutcome(null);
    } catch (e) {
      noteOutcome(e);
      if (it.diff !== df) return;
      Object.assign(df.rec, { state: 'error', error: e.code || 'unknown' });
      if (FMT_DENIED.has(e.code)) { Object.assign(df, { state: 'error', error: e.code }); paint(); return; }
    }
    if (!revTextOn()) { Object.assign(df, { state: 'none' }); paint(); return; }
  }
  try {
    let base = 0;
    const tick = (at, total) => {
      df.got = base + at;
      const el = document.getElementById('df-progress');
      if (el) el.textContent = tx('正在读取旧版和新版… {0} KiB', (df.got / 1024).toFixed(1));
      void total;
    };
    const a = await readRevisionText(it, df.from, target, df.fromSha, tick);
    if (it.diff !== df) return;
    base = U8E.encode(a).length;
    const b = await readRevisionText(it, df.to, target, df.toSha, tick);
    if (it.diff !== df) return;
    Object.assign(df, { state: 'ready', result: diffTexts(a, b) });
    noteOutcome(null);
  } catch (e) {
    noteOutcome(e);
    if (it.diff !== df) return;
    Object.assign(df, { state: 'error', error: e.code || 'unknown' });
  }
  paint();
}
// Myers' O(ND) shortest edit script on two arrays of ints; null when it needs more than maxD edits.
function myers(a, b, maxD) {
  const n = a.length; const m = b.length; const max = Math.min(n + m, maxD); const off = max + 1;
  const v = new Int32Array(2 * max + 3); const trace = [];
  for (let d = 0; d <= max; d += 1) {
    trace.push(v.slice(off - d - 1, off + d + 2));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[off + k - 1] < v[off + k + 1]) ? v[off + k + 1] : v[off + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) { x += 1; y += 1; }
      v[off + k] = x;
      if (x >= n && y >= m) {
        // Walk the trace back into an edit script: ['eq'|'del'|'ins', ai, bi].
        const ops = []; let cx = n; let cy = m;
        for (let dd = d; dd > 0; dd -= 1) {
          const pv = trace[dd]; const at = (kk) => pv[kk + dd + 1];
          const kk = cx - cy;
          const prevK = kk === -dd || (kk !== dd && at(kk - 1) < at(kk + 1)) ? kk + 1 : kk - 1;
          const px = at(prevK); const py = px - prevK;
          while (cx > px && cy > py) { cx -= 1; cy -= 1; ops.push(['eq', cx, cy]); }
          if (cx === px) { cy -= 1; ops.push(['ins', cx, cy]); } else { cx -= 1; ops.push(['del', cx, cy]); }
        }
        while (cx > 0 && cy > 0) { cx -= 1; cy -= 1; ops.push(['eq', cx, cy]); }
        return ops.reverse();
      }
    }
  }
  return null;
}
// Edit script of two token arrays (strings): common prefix/suffix first, Myers on the middle; when
// the middle needs too many edits it is shown as one deletion and one insertion (still exact).
function diffSeq(a, b, maxD) {
  let p = 0; while (p < a.length && p < b.length && a[p] === b[p]) p += 1;
  let s = 0; while (s < a.length - p && s < b.length - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s += 1;
  const ids = new Map(); const id = (t) => { let x = ids.get(t); if (x === undefined) { x = ids.size; ids.set(t, x); } return x; };
  const ma = a.slice(p, a.length - s).map(id); const mb = b.slice(p, b.length - s).map(id);
  let mid = myers(ma, mb, maxD); let coarse = false;
  if (!mid) { coarse = true; mid = [...ma.map((_, i) => ['del', i, 0]), ...mb.map((_, j) => ['ins', ma.length, j])]; }
  const ops = [];
  for (let i = 0; i < p; i += 1) ops.push(['eq', i, i]);
  for (const [t, i, j] of mid) ops.push([t, i + p, j + p]);
  for (let i = 0; i < s; i += 1) ops.push(['eq', a.length - s + i, b.length - s + i]);
  return { ops, coarse };
}
const TOKEN = /[A-Za-z0-9_]+|\s+|[\s\S]/gu;
// Changed words inside a deleted/inserted line pair: runs of [text, changed] per side, or null when
// the two lines share too little to be read as one modified line.
function intraLine(a, b) {
  const ta = a.match(TOKEN) || []; const tb = b.match(TOKEN) || [];
  const { ops, coarse } = diffSeq(ta, tb, 1500);
  if (coarse) return null;
  let same = 0; const ra = []; const rb = [];
  const push = (runs, text, x) => { const last = runs[runs.length - 1]; if (last && last[1] === x) last[0] += text; else runs.push([text, x]); };
  for (const [t, i, j] of ops) {
    if (t === 'eq') { same += ta[i].length; push(ra, ta[i], false); push(rb, tb[j], false); } else if (t === 'del') push(ra, ta[i], true); else push(rb, tb[j], true);
  }
  return 2 * same >= 0.3 * (a.length + b.length) ? { a: ra, b: rb } : null;
}
// Rows of the whole comparison: {t:'eq'|'del'|'ins', a, b (line numbers), text, runs?}; inside a
// change block, paired deleted/inserted lines come one after the other with their in-line runs.
function diffTexts(oldText, newText) {
  const split = (x) => x.split('\n');
  let la = split(oldText); let lb = split(newText);
  if (oldText.endsWith('\n') && newText.endsWith('\n')) { la = la.slice(0, -1); lb = lb.slice(0, -1); }
  const { ops, coarse } = diffSeq(la, lb, 2000);
  const rows = []; let dels = 0; let inss = 0;
  for (let k = 0; k < ops.length;) {
    if (ops[k][0] === 'eq') { rows.push({ t: 'eq', a: ops[k][1] + 1, b: ops[k][2] + 1, text: la[ops[k][1]] }); k += 1; continue; }
    const del = []; const ins = [];
    for (; k < ops.length && ops[k][0] !== 'eq'; k += 1) (ops[k][0] === 'del' ? del : ins).push(ops[k]);
    dels += del.length; inss += ins.length;
    const n = Math.min(del.length, ins.length);
    for (let i = 0; i < Math.max(del.length, ins.length); i += 1) {
      const runs = i < n ? intraLine(la[del[i][1]], lb[ins[i][2]]) : null;
      if (i < del.length) rows.push({ t: 'del', a: del[i][1] + 1, text: la[del[i][1]], runs: runs?.a, mod: !!runs });
      if (i < ins.length) rows.push({ t: 'ins', b: ins[i][2] + 1, text: lb[ins[i][2]], runs: runs?.b, mod: !!runs });
    }
  }
  return { rows, dels, inss, coarse, same: dels === 0 && inss === 0 };
}
// Unchanged runs longer than the context are folded to 「… n 行未改动」 (tap to open).
function diffBlocks(df) {
  const rows = df.result.rows; const out = [];
  for (let i = 0; i < rows.length;) {
    if (rows[i].t !== 'eq') { out.push({ row: rows[i], i }); i += 1; continue; }
    let j = i; while (j < rows.length && rows[j].t === 'eq') j += 1;
    const head = i === 0 ? 0 : DIFF_CONTEXT; const tail = j === rows.length ? 0 : DIFF_CONTEXT;
    if (j - i > head + tail + 1 && !df.open.has(i)) {
      for (let x = i; x < i + head; x += 1) out.push({ row: rows[x], i: x });
      out.push({ fold: j - i - head - tail, at: i });
      for (let x = j - tail; x < j; x += 1) out.push({ row: rows[x], i: x });
    } else for (let x = i; x < j; x += 1) out.push({ row: rows[x], i: x });
    i = j;
  }
  return out;
}
function diffLine(r, i, no) {
  const txt = h('span', { class: 'df-txt' });
  if (r.runs) for (const [t, x] of r.runs) txt.append(x ? h('span', { class: 'df-x' }, t) : t);
  else txt.append(r.text);
  if (!r.text) txt.append(h('span', { class: 'df-empty', 'aria-hidden': 'true' }, '\u00a0'));
  const sign = r.t === 'del' ? '−' : r.t === 'ins' ? '+' : '';
  // Synced 并排 (bindSync) follows unchanged rows only; changed rows never drive the other pane.
  return h('div', { class: `df-line df-${r.t}${r.mod ? ' df-mod' : ''} al-seg`, 'data-row': String(i), 'data-rel': r.t === 'eq' ? 'matched' : 'original_only' },
    h('span', { class: 'df-no' }, String(no ?? '')), h('span', { class: 'df-sign', 'aria-label': r.t === 'del' ? tx('删除') : r.t === 'ins' ? tx('新增') : null }, sign), txt);
}
function diffPane(it, df, which) {
  const lines = diffBlocks(df).flatMap((x) => {
    if (x.fold != null) return [h('button', { class: 'df-fold', onclick: () => { df.open.add(x.at); render(); } }, tx('… {0} 行未改动', x.fold))];
    const r = x.row;
    if (which === 'old' && r.t === 'ins') return [];
    if (which === 'new' && r.t === 'del') return [];
    return [diffLine(r, x.i, r.t === 'del' ? r.a : r.t === 'ins' ? r.b : which === 'old' ? r.a : r.b)];
  });
  const article = h('div', { class: `df-text${fontCls(it)}`, lang: (it.book?.doc && df.side && bookEd(it.book, df.side).language) || null }, lines);
  return h('div', { class: 'scroll book-pane df-pane', 'data-df': which }, article);
}
function diffSheet(it) {
  const df = it.diff;
  if (!df || (!it.book?.doc && df.target.kind !== 'body' && !it.host)) return [];
  const close = () => { it.diff = null; render(); };
  const where = df.target.kind === 'body' ? tx('正文') : sideName(df.side);
  const lay = state.diffLayout;
  const seg = h('div', { class: 'seg3 df-lay', role: 'group', 'aria-label': tx('对比方式') }, [['unified', tx('合并')], ['side', tx('并排')]].map(([k, label]) => h('button', {
    class: lay === k ? 'on' : '', 'aria-pressed': String(lay === k), 'data-layout': k,
    onclick: () => { state.diffLayout = k; localStorage.setItem('memex.m.diffLayout', k); render(); },
  }, label)));
  let body;
  if (df.rec?.open) body = fmtRecords(it, df);
  else if (df.state === 'none') body = h('div', { class: 'df-wait' }, tx('这台 Mac 不提供旧版文字，只能查看整理记录'));
  else if (df.state === 'loading') body = h('div', { class: 'df-wait' }, h('span', { class: 'spin', 'aria-hidden': 'true' }), h('span', { id: 'df-progress' }, tx('正在读取旧版和新版… {0} KiB', (df.got / 1024).toFixed(1))));
  else if (df.state === 'error') body = h('div', { class: 'df-wait bad' }, h('span', { class: 'grow' }, tx('没有显示对比：{0}', fmtErr(df.error))), FMT_DENIED.has(df.error) ? null : h('button', { class: 'btn-sm', onclick: () => openDiff(it, df) }, tx('重试')));
  else if (df.result.same) body = h('div', { class: 'df-wait' }, df.target.kind === 'body' ? tx('两个版本的正文相同，没有改动') : tx('两个版本的这一章相同，没有改动'));
  else if (lay === 'side') {
    const half = (which, label) => h('section', { class: `book-half df-half ${which}`, 'aria-label': label }, h('div', { class: 'book-label' }, label), diffPane(it, df, which));
    const halves = [half('old', tx('旧版')), half('new', tx('新版'))];
    body = h('div', { class: 'book-split df-split' }, halves);
    bindSync(...halves.map((x) => x.querySelector('.book-pane')));
  } else body = h('div', { class: 'df-one' }, h('div', { class: 'df-cap' }, h('span', { class: 'df-tag del' }, tx('旧版')), '→', h('span', { class: 'df-tag ins' }, tx('新版'))), diffPane(it, df, 'both'));
  const counts = df.state === 'ready' && !df.result.same ? tx('删除 {0} 行 · 新增 {1} 行', df.result.dels, df.result.inss) : null;
  return [h('section', { class: 'sheet df-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('对比修改'), 'data-state': df.state, 'data-layout': lay },
    ribbon(), h('header', { class: 'ce-head' },
      h('button', { class: 'icon-btn', 'aria-label': tx('关闭对比'), onclick: close }, icon('x', 20)),
      h('div', { class: 'ce-title' }, h('div', { class: 'ce-what' }, tx('对比修改')), h('div', { class: 'ce-sub' }, `${where} · ${df.title}`))),
    h('div', { class: 'df-bar' }, h('span', { class: 'df-legend grow' }, h('i', { class: 'df-key del' }), h('i', { class: 'df-key ins' }), tx('红色为删除 · 绿色为新增')), df.rec?.open ? null : seg),
    df.rec ? h('div', { class: 'df-recbar' }, h('button', { class: `btn-sm fr-toggle${df.rec.open ? ' on' : ''}`, 'aria-pressed': String(!!df.rec.open), disabled: !revTextOn() && df.rec.open,
      onclick: () => { df.rec.open = !df.rec.open; render(); } }, df.rec.open ? tx('返回对比') : df.rec.state === 'ready' ? tx('整理记录（{0} 处）', df.rec.total) : tx('整理记录'))) : null,
    counts || df.result?.coarse ? h('div', { class: 'df-count' }, counts, df.result?.coarse ? ` · ${tx('改动较多，部分段落按整段标出删除与新增')}` : '') : null,
    body)];
}

// ---------- 格式整理 (reader_format / reader_format_batch): unified content formatting ----------
// One item's body or one side of one chapter (a single-target batch), or a folder (batch). A local
// preview gives a server-owned quote; one explicit confirmation (Private needs its own consent)
// sends format_request with a request id stored first. A lost or malformed reply is unknown: only
// 查询原任务 replays the SAME request (it returns the original batch); nothing is resent by itself.
// Succeeded targets are kept as handles (batch, target, revisions, proof version), so 查看修改 and
// 退回旧版 work without re-reading the batch; a whole-batch read that is refused shows no aggregate
// and no target from cache — known handles are shown only after the Mac authorizes each again.
const fmtOn = () => state.caps?.reader_format === 'available' && !!state.caps?.scopes?.includes('edit');
const fmtBatchOn = () => fmtOn() && state.caps?.reader_format_batch === 'available';
const FMT_NS = GATEWAY ? 'gw' : 'demo';
const FMT_PAGE = 20;
const FMT_ACTIVE = new Set(['sending', 'polling', 'unknown', 'reading']);
// The Mac refused to read the whole batch (a member is no longer accessible, or the batch is not visible).
const FMT_DENIED = new Set(['library_resource_not_found', 'item_not_found', 'permission_denied', 'content_format_batch_not_found']);
// No answer: the write may have happened. library_loading is not a definite rejection either.
const FMT_UNKNOWN = new Set(['unknown', 'unavailable', MAC_BUSY, 'invalid_response']);
const tKey = (t) => (t.kind === 'body' ? 'body' : `${t.side}:${t.chapter}`);
const sameT = (a, b) => !!a && !!b && tKey(a) === tKey(b);
const fmtReqKey = (sel) => (sel.kind === 'item' ? ['memex.m.fmtReq', FMT_NS, sel.item_id, sel.revision_id, tKey(sel.target)]
  : ['memex.m.fmtBatch', FMT_NS, sel.group_id, sel.include_descendants ? 'tree' : 'direct']).join('|');
const FMT_HANDLES = `memex.m.fmtHandles|${FMT_NS}`;
const fmtRestoreKey = (hd) => ['memex.m.fmtRestore', FMT_NS, hd.batch_id, hd.target_id].join('|');
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const FMT_ERR = {
  // Codes of the contract (Unified content formatting v1); content_format_preview_timeout is local.
  content_format_quote_stale: tx('确认已过期或设置已变化，请重新确认'),
  content_format_quote_consumed: tx('这次确认已经用过，请重新确认'),
  content_format_unavailable: tx('格式整理模型暂不可用，或没有给出可用的结果'),
  content_format_invalid_result: tx('模型给出的整理结果没有通过校验，正文没有改动'),
  content_format_restore_conflict: tx('条目已在别处更新，不能退回旧版'),
  content_format_changes_stale: tx('整理记录已变化，请重新打开'),
  content_format_preview_not_found: tx('整理计划已失效，请重新发起'),
  content_format_batch_not_found: tx('Mac 上找不到这次整理任务'),
  content_format_preview_timeout: tx('Mac 准备整理计划的时间太长，请稍后再试'),
  no_translation_provider: tx('Mac 上还没有配置整理所用的模型'),
  provider_outcome_unknown: tx('不确定模型请求是否完成'),
  stale_item_revision: tx('条目已在 Mac 上更新，请刷新'),
  request_identity_conflict: tx('这次整理请求与之前的记录不一致，请重新发起'),
  request_id_conflict: tx('这次整理请求与之前的记录不一致，请重新发起'),
  permission_denied: tx('这次配对没有编辑权限，不能整理'),
  library_resource_not_found: tx('条目不存在或无权访问'),
  unknown: tx('没有收到 Mac 的回复'),
  job_unknown: tx('不确定模型请求是否完成'),
};
const fmtErr = (code) => FMT_ERR[code] || EDIT_ERR[code] || BOOK_ERR[code] || tx('原因：{0}', code);
const FMT_REASON = {
  already_formatted: tx('已整理过'), chapter_required: tx('书籍需按章整理'), restricted: tx('受限内容不发给模型'),
  budget_exceeded: tx('超出预算'), empty: tx('没有正文'), not_text: tx('没有可整理的文字'), trashed: tx('已在垃圾桶'),
  unsupported: tx('不支持'), revision_changed: tx('条目已更新'), unavailable: tx('暂不可用'),
};
const fmtReason = (r) => FMT_REASON[r] || r || '';
const FMT_KIND = { whitespace: tx('空白'), paragraph_boundary: tx('分段'), garbage_delete: tx('删除无关内容'), typo: tx('错字'), extraction_garble: tx('乱码'), punctuation: tx('标点') };
const FMT_WHY = { navigation: tx('导航'), advertisement: tx('广告'), saved_control: tx('网页控件'), account_widget: tx('账户组件'), paragraph_layout: tx('段落排版'),
  short_correction: tx('短修正'), source_garble: tx('来源乱码'), punctuation: tx('标点') };
const fmtScope = (t) => (t.kind === 'body' ? tx('本条正文') : t.side === 'translation' ? tx('本章译文') : tx('本章原文'));
const unknownText = () => tx('结果未知：不确定格式整理是否完成，没有显示不完整的结果。可以查询原任务，不会自动重试。');

// Handles of succeeded targets this device saw (the batch read authorized them then).
function fmtHandles() { try { const a = JSON.parse(localStorage.getItem(FMT_HANDLES) || '[]'); return Array.isArray(a) ? a : []; } catch { return []; } }
function fmtKeep(hd) {
  const all = fmtHandles().filter((x) => !(x.batch_id === hd.batch_id && x.target_id === hd.target_id));
  all.push(hd);
  try { localStorage.setItem(FMT_HANDLES, JSON.stringify(all.slice(-100))); } catch { /* the in-memory state still works */ }
}
function fmtForget(hd) { try { localStorage.setItem(FMT_HANDLES, JSON.stringify(fmtHandles().filter((x) => !(x.batch_id === hd.batch_id && x.target_id === hd.target_id)))); } catch { /* ignore */ } }
const handleOf = (batch_id, row, usage) => (row.state === 'succeeded' && row.job?.result_revision_id && row.formatting ? {
  batch_id, target_id: row.target_id, item_id: row.item_id, target: row.target, input_revision_id: row.revision_id, result_revision_id: row.job.result_revision_id,
  changes_version: row.formatting.changes_version, changed: row.formatting.changed, status: row.formatting.status, unresolved: row.formatting.unresolved_count,
  usage: usage ?? row.job.usage ?? null, restored: null } : null);
// The handle for the target on screen, at the revision on screen (the result, or its restore).
function handleHere(it, t) {
  const rev = it.meta?.revision_id;
  return fmtHandles().reverse().find((x) => x.item_id === it.id && sameT(x.target, t) && (x.result_revision_id === rev || x.restored?.revision_id === rev)) || null;
}

// The target on screen: an ordinary item's body, or the chapter of the side shown (not in 对照).
function fmtHere(it) {
  if (!it?.meta) return null;
  if (!it.book) return { kind: 'body', title: it.meta.title };
  if (!it.book.doc || state.lang === 'bi') return null;
  const side = state.lang === 'trans' ? 'translation' : 'original';
  const ch = bookChapter(it.book, side);
  return ch ? { kind: 'chapter', side, chapter: ch.key, title: ch.title.trim() } : null;
}
const bare = (t) => (t.kind === 'body' ? { kind: 'body' } : { kind: 'chapter', side: t.side, chapter: t.chapter });
const live = (o) => o === state.fmt || o === state.fmtBatch;
function repaint(o) {
  if (o === state.fmt && state.item?.id === o.item_id && route().name === 'item') softRender();
  else if (o === state.fmtBatch && route().name === 'library') softRender();
}

// 格式整理 in the reader: a fresh preview shown in the confirmation sheet; nothing is sent yet.
function askFormat(it) {
  const t = fmtHere(it);
  if (!t) { toast(it.book?.doc && state.lang === 'bi' ? tx('切到原文或译文后整理本章') : tx('正在载入本章…')); return; }
  const cur = state.fmt;
  if (cur && cur.item_id === it.id && ['sending', 'polling', 'unknown'].includes(cur.phase)) { toast(tx('这次整理还没有结束')); return; }
  clearTimeout(cur?.timer);
  const sel = { kind: 'item', item_id: it.id, revision_id: it.meta.revision_id, target: bare(t) };
  state.fmt = { item_id: it.id, sel, target: bare(t), title: t.title || it.meta.title, phase: 'ask', ask: { state: 'loading' } };
  render();
  fmtPreview(state.fmt);
}
function askBatch() {
  const q = state.lib.query;
  if (!q.group_id || !fmtBatchOn()) return;
  const sel = { kind: 'group', group_id: q.group_id, include_descendants: !!q.include_descendants };
  const cur = state.fmtBatch;
  if (cur && tKeySel(cur.sel) === tKeySel(sel) && cur.phase !== 'ask') {
    cur.open = true;
    // Reopened: the batch is read again (a member may have become inaccessible meanwhile).
    if (cur.batch?.batch_id && ['done', 'paused', 'denied'].includes(cur.phase)) Object.assign(cur, { phase: 'polling', rowsErr: null });
    render();
    if (cur.phase === 'polling' && !cur.timer) pollFormat(cur, 0);
    return;
  }
  clearTimeout(cur?.timer);
  const o = { sel, groupName: q.groupName, phase: 'ask', ask: { state: 'loading' }, rows: [], open: false };
  state.fmtBatch = o;
  // A batch this device already sent for this folder: read it (its id is known), or offer 查询原任务.
  let saved = null;
  try { saved = JSON.parse(localStorage.getItem(fmtReqKey(sel)) || 'null'); } catch { /* none */ }
  if (saved && REQUEST_ID.test(saved.request_id || '') && typeof saved.quote_id === 'string') {
    Object.assign(o, { key: fmtReqKey(sel), payload: { quote_id: saved.quote_id, request_id: saved.request_id }, ask: null, open: true });
    if (saved.batch_id) { o.phase = 'polling'; o.batch = { batch_id: saved.batch_id }; render(); pollFormat(o, 0); } else { Object.assign(o, { phase: 'unknown', error: 'unknown' }); render(); }
    return;
  }
  render();
  fmtPreview(o);
}
const tKeySel = (sel) => fmtReqKey(sel);
async function fmtPreview(o) {
  const ask = o.ask;
  try {
    let p = await source.readerFormatPreview({ selection: o.sel });
    for (let i = 0; p.state === 'preparing'; i += 1) {
      if (o.ask !== ask) return;
      if (i >= 150) throw codeErr('content_format_preview_timeout');
      await pause(400);
      p = await source.readerFormatPreview({ preview_id: p.preview_id });
    }
    noteOutcome(null);
    if (o.ask !== ask) return;
    if (p.state === 'failed') Object.assign(ask, { state: 'error', error: p.error_code || 'content_format_unavailable' });
    else {
      const q = p.quote; const c = q.counts;
      Object.assign(ask, { preview_id: p.preview_id, quote: q, consent: false,
        state: c.eligible > 0 && q.budget.max_requests >= 0 ? 'ready' : c.skipped_by_reason.already_formatted ? 'already' : 'none' });
      // A single target that needs nothing: said in the reader, no confirmation.
      if (o === state.fmt && ask.state === 'already') Object.assign(o, { phase: 'already', ask: null });
    }
  } catch (e) {
    noteOutcome(e);
    if (o.ask !== ask) return;
    Object.assign(ask, { state: 'error', error: e.code || 'unknown' });
  }
  repaint(o);
}
// 确认整理: the request id and quote are stored before anything is sent.
function confirmFormat(o) {
  const q = o.ask?.quote;
  if (!q || o.ask.state !== 'ready' || (q.counts.private_targets > 0 && !o.ask.consent)) return;
  if (Date.now() > q.expires_at - 5000) { o.ask = { state: 'loading' }; render(); fmtPreview(o); return; }
  const payload = { quote_id: q.quote_id, request_id: freshRequestId() };
  const key = fmtReqKey(o.sel);
  try { localStorage.setItem(key, JSON.stringify(payload)); } catch { toast(tx('无法在这台设备上记下整理请求，没有发送')); return; }
  Object.assign(o, { payload, key, quote: q, ask: null, open: o === state.fmtBatch, rows: [], total: null, next: null, denied: false });
  sendFormat(o);
}
// Also 查询原任务: the SAME request id and quote (the Mac returns the original batch).
async function sendFormat(o) {
  const replay = o.phase === 'unknown';
  Object.assign(o, { phase: 'sending', error: null });
  repaint(o);
  try {
    const batch = await source.readerFormatRequest({ ...o.payload, accept_cost: true, accept_model_processing: true });
    noteOutcome(null);
    if (!live(o)) return;
    if (o === state.fmtBatch) try { localStorage.setItem(o.key, JSON.stringify({ ...o.payload, batch_id: batch.batch_id })); } catch { /* the id is still in memory */ }
    applyBatch(o, batch);
  } catch (e) {
    noteOutcome(e);
    if (!live(o)) return;
    const code = e.code || 'unknown';
    // After an unknown attempt, a session that cannot check now says nothing about the outcome.
    if (FMT_UNKNOWN.has(code) || (replay && EDIT_SESSION.has(code))) Object.assign(o, { phase: 'unknown', error: code });
    else { localStorage.removeItem(o.key); Object.assign(o, { phase: 'rejected', error: code }); }
  }
  repaint(o);
}
function applyBatch(o, batch) {
  o.batch = batch;
  if (batch.state === 'queued' || batch.state === 'running') { o.phase = 'polling'; pollFormat(o); return; }
  o.phase = 'reading';
  readResults(o, 0);
}
const fmtShown = (o) => (o === state.fmt ? state.item?.id === o.item_id && route().name === 'item' : o === state.fmtBatch && o.open && route().name === 'library');
// Status every 1.5 s while the target or the batch sheet is on screen (a read: bounded busy retry).
function pollFormat(o, wait = 1500) {
  clearTimeout(o.timer);
  o.timer = setTimeout(async () => {
    o.timer = null;
    if (!live(o) || !fmtShown(o)) return;
    try {
      const batch = await source.readerFormatStatus({ batch_id: o.batch.batch_id });
      noteOutcome(null); o.lost = null;
      if (!live(o)) return;
      applyBatch(o, batch);
    } catch (e) {
      noteOutcome(e);
      if (!live(o)) return;
      if (FMT_DENIED.has(e.code)) fmtDenied(o, e.code); else { o.lost = e.code || 'unknown'; pollFormat(o); }
    }
    repaint(o);
  }, wait);
}
// The whole batch cannot be read: no counts, usage or rows (nothing from cache); not cancelled or
// deleted. Known succeeded targets are offered only after each is authorized again (format_changes).
function fmtDenied(o, code) {
  clearTimeout(o.timer);
  Object.assign(o, { phase: 'denied', error: code, rows: [], total: null, next: null, verified: null, batch: o.batch && { batch_id: o.batch.batch_id } });
  if (o !== state.fmtBatch) return;
  const known = fmtHandles().filter((x) => x.batch_id === o.batch?.batch_id);
  (async () => {
    const ok = [];
    for (const hd of known) {
      try { await fmtChangesPage(hd, 0, 1); ok.push(hd); } catch { /* not shown now; kept so a later authorized read can offer it again */ }
      if (!live(o)) return;
    }
    o.verified = ok; repaint(o);
  })();
}
async function readResults(o, offset) {
  o.loadingRows = true;
  try {
    const page = await source.readerFormatResults({ batch_id: o.batch.batch_id, offset, limit: o === state.fmt ? 1 : FMT_PAGE });
    noteOutcome(null);
    if (!live(o)) return;
    const single = o === state.fmt;
    for (const row of page.results) {
      const hd = handleOf(o.batch.batch_id, row, single ? o.batch.usage : null);
      const prev = hd && fmtHandles().find((x) => x.batch_id === hd.batch_id && x.target_id === hd.target_id);
      if (hd) fmtKeep({ ...hd, restored: prev?.restored ?? null });
    }
    o.rows = offset === 0 ? page.results : [...o.rows, ...page.results];
    Object.assign(o, { total: page.total, next: page.next_offset, loadingRows: false });
    if (single) finishSingle(o, page.results[0]);
    else { o.phase = o.batch.state === 'paused' ? 'paused' : 'done'; }
  } catch (e) {
    noteOutcome(e);
    if (!live(o)) return;
    o.loadingRows = false;
    if (FMT_DENIED.has(e.code)) fmtDenied(o, e.code);
    else Object.assign(o, { phase: o === state.fmt ? 'reading' : o.phase === 'reading' ? 'done' : o.phase, rowsErr: e.code || 'unknown' });
  }
  repaint(o);
}
function finishSingle(o, row) {
  o.row = row || null;
  if (!row) { Object.assign(o, { phase: 'failed', error: 'invalid_response' }); return; }
  if (row.state === 'outcome_unknown') { Object.assign(o, { phase: 'unknown', error: 'job_unknown' }); return; }
  if (row.state === 'queued' || row.state === 'running') { o.phase = o.batch.state === 'paused' ? 'paused' : 'polling'; if (o.phase === 'polling') pollFormat(o); return; }
  localStorage.removeItem(o.key);
  if (row.state === 'succeeded') {
    o.handle = handleOf(o.batch.batch_id, row, o.batch.usage);
    o.phase = o.handle?.changed ? 'done' : 'already';
    if (o.handle?.changed) fmtReread(o.item_id);
    return;
  }
  if (row.state === 'skipped') { Object.assign(o, { phase: row.reason === 'already_formatted' ? 'already' : 'rejected', error: row.reason || 'content_format_unavailable' }); return; }
  Object.assign(o, { phase: 'failed', error: row.job?.error_code || 'content_format_unavailable' });
}
// After a new revision: read the item again (a book keeps its chapter, mode, editor and drafts).
async function fmtReread(item_id) {
  const it = state.item;
  if (!it || it.id !== item_id || route().name !== 'item') return;
  if (!it.book) { loadItem(it.id); return; }
  try { await reloadBook(it); noteOutcome(null); } catch (e) { if (e.code !== 'cancelled') { noteOutcome(e); toast(bookErr(e.code || 'unknown')); } }
  if (state.item === it) softRender();
}
// 查看修改: the old and new text of that target (both checked by hash) and the proof list.
function fmtChangesPage(hd, offset, limit = 64) {
  return source.readerFormatChanges({ batch_id: hd.batch_id, target_id: hd.target_id, expected_version: hd.changes_version, view: 'list', offset, limit })
    .then((p) => { if (p.item_id !== hd.item_id || p.input_revision_id !== hd.input_revision_id || p.result_revision_id !== hd.result_revision_id) throw codeErr('invalid_response'); return p; });
}
function viewChanges(host, hd, title) {
  if (host.host) host.id = hd.item_id;
  openDiff(host, { title, target: hd.target, from: hd.input_revision_id, to: hd.result_revision_id, fmt: hd });
}
async function moreRecords(it, df) {
  const rec = df.rec;
  if (rec.next == null || rec.loading) return;
  rec.loading = true; render();
  try { const p = await fmtChangesPage(df.fmt, rec.next); rec.rows.push(...p.changes); rec.next = p.next_offset; noteOutcome(null); } catch (e) { noteOutcome(e); toast(fmtErr(e.code || 'unknown')); }
  rec.loading = false; if (it.diff === df) render();
}
// One part of one change, by bounded pieces (≤8 KiB, whole code points); at most 64 KiB is shown.
async function changeText(hd, change_id, part) {
  let text = ''; let start = 0; let total = 0;
  for (let i = 0; i < 9; i += 1) {
    const r = await source.readerFormatChanges({ batch_id: hd.batch_id, target_id: hd.target_id, expected_version: hd.changes_version, view: 'text', change_id, part, start_byte: start, limit_bytes: 8192 });
    text += r.text; total = r.total_bytes; start = r.end_byte;
    if (r.next_offset == null || start >= 65536) break;
  }
  return { text, total, cut: start < total };
}
async function openRecord(it, df, c) {
  const t = df.rec.texts;
  if (t.has(c.id)) { t.delete(c.id); render(); return; }
  const x = { state: 'loading' }; t.set(c.id, x); render();
  try {
    const [before, after] = await Promise.all([c.before_bytes ? changeText(df.fmt, c.id, 'before') : { text: '', total: 0 }, c.after_bytes ? changeText(df.fmt, c.id, 'after') : { text: '', total: 0 }]);
    Object.assign(x, { state: 'ready', before, after }); noteOutcome(null);
  } catch (e) { noteOutcome(e); Object.assign(x, { state: 'error', error: e.code || 'unknown' }); }
  if (it.diff === df) render();
}
function fmtRecords(it, df) {
  const rec = df.rec;
  if (rec.state === 'loading') return h('div', { class: 'df-wait' }, h('span', { class: 'spin', 'aria-hidden': 'true' }), tx('正在读取整理记录…'));
  if (rec.state === 'error') return h('div', { class: 'df-wait bad' }, tx('整理记录没有读取成功：{0}', fmtErr(rec.error)));
  if (!rec.rows.length) return h('div', { class: 'df-wait' }, tx('这次整理没有改动'));
  // Whitespace-only parts (a paragraph break, spaces) are drawn as marks so the edit is visible.
  const marks = (t) => t.replace(/\n/g, '↵').replace(/\t/g, '⇥').replace(/ /g, '␣');
  const shown = (s, cut) => [!s.text ? h('span', { class: 'fr-none' }, tx('（空）')) : !s.text.trim() ? h('span', { class: 'fr-ws' }, marks(s.text)) : h('span', { class: 'fr-t' }, s.text),
    cut ? h('span', { class: 'fr-none' }, ` ${tx('… 只显示前 64 KiB')}`) : null];
  return h('div', { class: 'scroll fr-list' }, rec.rows.map((c) => {
    const x = rec.texts.get(c.id);
    const size = c.kind === 'garbage_delete' || !c.after_bytes ? tx('删除 {0} 字节', c.before_bytes) : !c.before_bytes ? tx('新增 {0} 字节', c.after_bytes) : tx('{0} → {1} 字节', c.before_bytes, c.after_bytes);
    return h('div', { class: 'fr-row', 'data-kind': c.kind },
      h('button', { class: 'fr-head', 'aria-expanded': String(!!x), onclick: () => openRecord(it, df, c) },
        h('span', { class: 'fr-kind' }, FMT_KIND[c.kind] || c.kind), c.reason_code ? h('span', { class: 'fr-why' }, FMT_WHY[c.reason_code] || c.reason_code) : null,
        h('span', { class: 'grow' }), h('span', { class: 'fr-size' }, size)),
      !x ? null : x.state === 'loading' ? h('div', { class: 'fr-body' }, h('span', { class: 'spin', 'aria-hidden': 'true' }))
        : x.state === 'error' ? h('div', { class: 'fr-body bad' }, fmtErr(x.error))
          : h('div', { class: 'fr-body' }, h('div', { class: 'fr-part del' }, h('span', { class: 'df-tag del' }, tx('旧版')), h('div', { class: 'fr-txt' }, shown(x.before, x.before.cut))),
            h('div', { class: 'fr-part ins' }, h('span', { class: 'df-tag ins' }, tx('新版')), h('div', { class: 'fr-txt' }, shown(x.after, x.after.cut)))));
  }), rec.next != null ? h('button', { class: 'btn fr-more', disabled: !!rec.loading, onclick: () => moreRecords(it, df) }, tx('继续载入')) : null);
}

// 退回旧版: that target's bytes before formatting, into a NEW revision (CAS on the result revision).
// The request id is stored first; an unknown outcome offers only 查询原任务 with the same id.
state.fmtRestore ??= {};
function restoreState(hd) {
  const key = fmtRestoreKey(hd);
  let rs = state.fmtRestore[key];
  if (!rs) {
    const rid = localStorage.getItem(key);
    if (rid && REQUEST_ID.test(rid)) rs = state.fmtRestore[key] = { phase: 'unknown', request_id: rid, error: 'unknown' };
  }
  return rs || null;
}
async function restoreFormat(hd) {
  const key = fmtRestoreKey(hd);
  let rs = restoreState(hd);
  const replay = rs?.phase === 'unknown';
  if (!replay) {
    if (!confirm(tx('退回格式整理之前的版本吗？会建立一个新版本，整理后的版本仍保留。'))) return;
    rs = state.fmtRestore[key] = { request_id: freshRequestId() };
    try { localStorage.setItem(key, rs.request_id); } catch { delete state.fmtRestore[key]; toast(tx('无法在这台设备上记下退回请求，没有发送')); return; }
  }
  Object.assign(rs, { phase: 'sending', error: null }); softRender();
  try {
    const r = await source.readerFormatRestore({ batch_id: hd.batch_id, target_id: hd.target_id, expected_revision_id: hd.result_revision_id, request_id: rs.request_id });
    noteOutcome(null);
    if (r.item_id !== hd.item_id) throw codeErr('unknown');
    localStorage.removeItem(key);
    Object.assign(rs, { phase: 'done', receipt: r });
    const kept = fmtHandles().find((x) => x.batch_id === hd.batch_id && x.target_id === hd.target_id) || hd;
    fmtKeep({ ...kept, restored: { revision_id: r.revision_id, changed: r.changed } });
    hd.restored = { revision_id: r.revision_id, changed: r.changed };
    fmtReread(hd.item_id);
  } catch (e) {
    noteOutcome(e);
    const code = e.code || 'unknown';
    if (FMT_UNKNOWN.has(code) || (replay && EDIT_SESSION.has(code))) Object.assign(rs, { phase: 'unknown', error: code });
    else { localStorage.removeItem(key); Object.assign(rs, { phase: 'rejected', error: code }); }
  }
  softRender();
}
// 继续整理 (a paused batch): never automatic; a refusal leaves it as it was (nothing requeued).
async function resumeFormat(o) {
  if (!o.batch?.batch_id || o.resuming) return;
  o.resuming = true; o.resumeErr = null; repaint(o);
  try { const batch = await source.readerFormatResume({ batch_id: o.batch.batch_id }); noteOutcome(null); if (live(o)) applyBatch(o, batch); } catch (e) {
    noteOutcome(e);
    if (live(o)) { o.resumeErr = e.code || 'unknown'; if (FMT_DENIED.has(e.code)) fmtDenied(o, e.code); }
  }
  o.resuming = false; repaint(o);
}

// Actions of one succeeded target: 查看修改 / 退回旧版, or how its restore went.
function handleActs(host, hd, title) {
  const act = (label, onclick, cls = 'ab-act') => h('button', { class: cls, onclick }, label);
  const rs = restoreState(hd);
  if (hd.restored || rs?.phase === 'done') return [h('span', { class: 'fr-done' }, tx('已退回旧版'))];
  if (rs?.phase === 'sending') return [h('span', { class: 'spin', 'aria-hidden': 'true' }), h('span', { class: 'fr-done' }, tx('正在退回旧版…'))];
  if (rs?.phase === 'unknown') return [act(tx('查询原任务'), () => restoreFormat(hd))];
  return [act(tx('查看修改'), () => viewChanges(host, hd, title)), hd.changed ? act(tx('退回旧版'), () => restoreFormat(hd)) : null];
}
function restoreLine(hd) {
  const rs = restoreState(hd);
  if (rs?.phase === 'unknown') return tx('退回结果未知：不确定是否已经退回旧版。可以查询原任务，不会自动重试。');
  if (rs?.phase === 'rejected') return tx('没有退回旧版：{0}', fmtErr(rs.error));
  return null;
}
// The banner above the target on screen.
function fmtBanner(it) {
  if (!it?.meta) return null;
  const shownT = it.book?.doc ? bookShown(it.book).map(([side, c]) => ({ kind: 'chapter', side, chapter: c.key, title: c.title.trim() })) : it.book ? [] : [{ kind: 'body', title: it.meta.title }];
  let o = state.fmt && state.fmt.item_id === it.id && shownT.some((t) => sameT(t, state.fmt.target)) && state.fmt.phase !== 'ask' ? state.fmt : null;
  let hd = o?.handle || null;
  if (!o && fmtOn()) {
    for (const t of shownT) {
      hd = handleHere(it, t);
      if (hd) break;
      // A stored request whose outcome was never confirmed (e.g. the page reloaded): 查询原任务 only.
      const sel = { kind: 'item', item_id: it.id, revision_id: it.meta.revision_id, target: bare(t) };
      let saved = null; try { saved = JSON.parse(localStorage.getItem(fmtReqKey(sel)) || 'null'); } catch { /* none */ }
      if (saved && REQUEST_ID.test(saved.request_id || '') && typeof saved.quote_id === 'string' && !(state.fmt && FMT_ACTIVE.has(state.fmt.phase))) {
        o = state.fmt = { item_id: it.id, sel, target: bare(t), title: t.title, key: fmtReqKey(sel), payload: { quote_id: saved.quote_id, request_id: saved.request_id }, phase: 'unknown', error: 'unknown' };
        break;
      }
    }
  }
  if (!o && !hd) return null;
  if (o?.phase === 'polling' && !o.timer) pollFormat(o);
  const act = (label, onclick) => h('button', { class: 'ab-act', onclick }, label);
  const spin = () => h('span', { class: 'spin', 'aria-hidden': 'true' });
  const grow = (...kids) => h('span', { class: 'grow' }, ...kids);
  const phase = o ? o.phase : 'done';
  const title = o?.title || shownT.find((t) => sameT(t, hd.target))?.title || it.meta.title;
  const usage = o?.batch?.usage ? usageText(o.batch.usage) : hd?.usage ? usageText(hd.usage) : null;
  let line; let details = null;
  if (phase === 'sending' || phase === 'polling' || phase === 'reading') {
    line = [spin(), grow(tx('正在整理格式…'), o.lost ? tx(' · 暂时连不上 Mac，稍后继续查看进度') : '')];
  } else if (phase === 'unknown') line = [grow(unknownText()), act(tx('查询原任务'), () => sendFormat(o))];
  else if (phase === 'done' && hd) {
    line = [grow(tx('格式已整理')), ...handleActs(it, hd, title)];
    details = [usage, hd.status === 'partial' && hd.unresolved ? tx('{0} 处没有把握，保持原样', hd.unresolved) : null, restoreLine(hd)].filter(Boolean).join(' · ') || null;
  } else if (phase === 'already') { line = [grow(tx('已整理过，无需再整理'))]; details = o?.batch ? usage : null; }
  else if (phase === 'paused') line = [grow(tx('格式整理已暂停')), act(tx('继续整理'), () => resumeFormat(o))];
  else if (phase === 'failed') { line = [grow(tx('格式整理没有成功')), act(tx('再试一次'), () => askFormat(it))]; details = [fmtErr(o.error), usage].filter(Boolean).join(' · '); }
  else if (phase === 'denied') line = [grow(fmtErr(o.error))];
  else line = [grow(o.error === 'already_formatted' ? tx('已整理过，无需再整理') : FMT_REASON[o.error] ? tx('没有整理：{0}', fmtReason(o.error)) : fmtErr(o.error)),
    o.error === 'stale_item_revision' ? act(tx('刷新'), () => fmtReread(it.id)) : act(tx('再试一次'), () => askFormat(it))];
  return h('div', { class: 'book-note align-banner fmt-banner', role: 'status', 'data-phase': phase },
    h('div', { class: `ab-row ab-line${['failed', 'rejected', 'denied'].includes(phase) ? ' bad' : ''}` }, line),
    details ? h('div', { class: 'ab-row ab-usage' }, details) : null);
}
// 格式整理前请确认: scope, skipped counts, model, planned requests, budget, Private consent.
function fmtConfirmSheet(o) {
  const ask = o?.ask;
  if (!ask || o.phase !== 'ask') return [];
  const q = ask.quote;
  const close = () => { if (o === state.fmt) state.fmt = null; else state.fmtBatch = null; render(); };
  const row = (k, v) => h('div', { class: 'xl-row' }, h('dt', {}, k), h('dd', {}, v));
  const scope = o.sel.kind === 'item' ? fmtScope(o.target) : tx('{0} 条', q ? q.counts.items : '…');
  const reasons = q ? Object.entries(q.counts.skipped_by_reason).map(([k, v]) => `${fmtReason(k)} ${v}`).join('、') : '';
  const priv = q && q.counts.private_targets > 0;
  const consent = priv ? h('label', { class: 'fmt-consent' }, h('input', { type: 'checkbox', checked: !!ask.consent, onchange: (e) => { ask.consent = e.target.checked; render(); } }),
    h('span', {}, tx('同意把所选正文发给模型'), h('span', { class: 'fmt-fine' }, tx('其中 {0} 个目标是私密内容', q.counts.private_targets)))) : null;
  const ready = ask.state === 'ready' && (!priv || ask.consent);
  return [h('div', { class: 'scrim', onclick: close }),
    h('section', { class: 'sheet repair-sheet xlate-sheet fmt-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('格式整理前请确认'), 'data-state': ask.state },
      h('div', { class: 'grab' }, h('span')), h('h2', {}, tx('格式整理前请确认')),
      h('div', { class: 'body' },
        h('p', { class: 'repair-ch' }, o.sel.kind === 'item' ? o.title : tx('分组：{0}', o.groupName || '')),
        h('p', {}, tx('整理分段、空白和标点，删除导航、广告等无关内容，并改正少量明显错字。整理后建立新版本，旧版保留，可以查看修改或退回旧版。')),
        ask.state === 'loading' ? h('p', { class: 'repair-fine' }, h('span', { class: 'spin', 'aria-hidden': 'true' }), ' ', tx('正在准备整理计划…'))
          : ask.state === 'error' ? h('p', { class: 'xl-bad' }, fmtErr(ask.error))
            : h('dl', { class: 'xl-quote' },
              row(tx('范围'), scope),
              o.sel.kind === 'group' ? row(tx('目标'), tx('{0} 个可整理', q.counts.eligible)) : null,
              q.counts.skipped ? row(tx('跳过'), `${q.counts.skipped}${reasons ? `（${reasons}）` : ''}`) : null,
              row(tx('模型'), q.route ? q.route.model_id : tx('不调用模型')),
              q.route ? row(tx('推理强度'), q.route.reasoning_effort || tx('默认')) : null,
              row(tx('计划请求'), tx('{0} 次（上限）', q.budget.max_requests)),
              row(tx('合计上限'), tx('输入 {0} 字节 · 输出 {1} tokens', q.budget.max_input_bytes, q.budget.max_completion_tokens)),
              q.estimated_cost ? row(tx('估计费用'), tx('预估费用约 {0} {1}', q.estimated_cost.amount, q.estimated_cost.currency)) : null),
        ask.state === 'already' ? h('p', { class: 'fmt-already' }, tx('已整理过，无需再整理'))
          : ask.state === 'none' ? h('p', { class: 'xl-bad' }, tx('没有可以整理的内容')) : null,
        consent,
        h('p', { class: 'repair-fine' }, o.sel.kind === 'group' ? tx('改动的是条目本身，在其他分组里看到的也是整理后的版本。预算是上限，不是预计用量。失败或结果未知时不会自动重试。')
          : tx('预算是上限，不是预计用量。失败或结果未知时不会自动重试。'))),
      h('div', { class: 'sheet-foot' }, h('span', { class: 'state quiet' }),
        h('button', { class: 'btn', onclick: close }, tx('取消')),
        h('button', { class: 'btn primary', disabled: !ready, onclick: () => confirmFormat(o) }, tx('确认整理'))))];
}
// The library row under the count when a folder is selected: 含子文件夹 and 批量格式整理.
function libraryFormatRow() {
  const q = state.lib.query;
  if (!q.group_id || !fmtBatchOn()) return h('div', { class: 'fmt-lib', hidden: true });
  return h('div', { class: 'fmt-lib' },
    h('label', { class: 'fmt-desc' }, h('input', { type: 'checkbox', checked: !!q.include_descendants, onchange: (e) => { q.include_descendants = e.target.checked; state.lib.scroll = 0; loadLibrary(true); } }), tx('含子文件夹')),
    h('span', { class: 'grow' }),
    h('button', { class: 'btn-sm fmt-batch-open', onclick: askBatch }, tx('批量格式整理')));
}
const FMT_ROW_STATE = { queued: tx('排队中'), running: tx('正在整理'), succeeded: tx('格式已整理'), failed: tx('没有成功'), outcome_unknown: tx('结果未知'), skipped: tx('跳过') };
function batchSheet() {
  const o = state.fmtBatch;
  if (!o) return [];
  if (o.phase === 'ask') return fmtConfirmSheet(o);
  if (!o.open) return [];
  const host = (o.host ??= { host: true, id: null, diff: null });
  if (host.diff) return diffSheet(host);
  const close = () => { o.open = false; clearTimeout(o.timer); o.timer = null; render(); };
  const act = (label, onclick, cls = 'btn-sm') => h('button', { class: cls, onclick }, label);
  const c = o.batch?.counts;
  const titleOf = (id) => state.lib.rows.find((r) => r.item_id === id)?.title || id;
  const what = (t) => (t.kind === 'body' ? tx('正文') : `${sideName(t.side)} · ${t.chapter}`);
  let top;
  if (o.phase === 'sending' || o.phase === 'polling' || o.phase === 'reading') top = h('div', { class: 'ce-msg' }, h('span', { class: 'spin', 'aria-hidden': 'true' }),
    o.batch?.state === 'done' ? h('span', { class: 'grow' }, tx('正在读取整理结果…'))
      : h('span', { class: 'grow' }, tx('正在整理格式…'), c ? ` ${tx('已完成 {0}/{1} 个', c.succeeded + c.failed + c.outcome_unknown + c.skipped, c.total)}` : '', o.lost ? tx(' · 暂时连不上 Mac，稍后继续查看进度') : ''));
  else if (o.phase === 'unknown') top = h('div', { class: 'ce-msg bad' }, h('span', { class: 'grow' }, unknownText()), act(tx('查询原任务'), () => sendFormat(o)));
  else if (o.phase === 'denied') top = h('div', { class: 'ce-msg bad', 'data-phase': 'denied' }, h('span', { class: 'grow' }, tx('无法读取整批：其中有条目已不可访问')));
  else if (o.phase === 'rejected') top = h('div', { class: 'ce-msg bad' }, h('span', { class: 'grow' }, fmtErr(o.error)), act(tx('再试一次'), () => { state.fmtBatch = null; askBatch(); }));
  else top = h('div', { class: 'ce-msg' }, h('span', { class: 'grow' }, o.phase === 'paused' ? tx('格式整理已暂停') : tx('格式整理已完成'), ' · ',
    tx('成功 {0} · 没有成功 {1} · 结果未知 {2} · 跳过 {3}', c.succeeded, c.failed, c.outcome_unknown, c.skipped)),
  o.phase === 'paused' ? act(o.resuming ? tx('正在继续…') : tx('继续整理'), () => resumeFormat(o)) : null);
  const usage = o.batch?.usage && !['denied', 'unknown'].includes(o.phase) ? h('div', { class: 'df-count' }, usageText(o.batch.usage)) : null;
  const rowEl = (r) => {
    const hd = handleOf(o.batch.batch_id, r, null);
    const kept = hd && (fmtHandles().find((x) => x.batch_id === hd.batch_id && x.target_id === hd.target_id) || hd);
    const why = r.state === 'skipped' ? fmtReason(r.reason) : r.state === 'failed' ? fmtErr(r.job?.error_code || 'content_format_unavailable')
      : r.state === 'succeeded' && !r.formatting?.changed ? tx('已整理过，无需再整理') : null;
    return h('div', { class: 'fb-row', 'data-state': r.state, 'data-item': r.item_id },
      h('div', { class: 'fb-main' }, h('div', { class: 'fb-title' }, titleOf(r.item_id)), h('div', { class: 'fb-sub' }, what(r.target), ' · ', FMT_ROW_STATE[r.state] || r.state, why ? ` · ${why}` : '')),
      kept && kept.changed ? h('div', { class: 'fb-acts' }, handleActs(host, kept, titleOf(r.item_id))) : null,
      kept && restoreLine(kept) ? h('div', { class: 'fb-note' }, restoreLine(kept)) : null);
  };
  const knownRow = (hd) => h('div', { class: 'fb-row', 'data-state': 'known', 'data-item': hd.item_id },
    h('div', { class: 'fb-main' }, h('div', { class: 'fb-title' }, titleOf(hd.item_id)), h('div', { class: 'fb-sub' }, what(hd.target), ' · ', tx('格式已整理'))),
    hd.changed ? h('div', { class: 'fb-acts' }, handleActs(host, hd, titleOf(hd.item_id))) : null,
    restoreLine(hd) ? h('div', { class: 'fb-note' }, restoreLine(hd)) : null);
  const list = o.phase === 'denied'
    ? [o.verified == null ? h('div', { class: 'df-wait' }, h('span', { class: 'spin', 'aria-hidden': 'true' }), tx('正在确认还能访问的整理结果…'))
      : o.verified.length ? [h('div', { class: 'fb-cap' }, tx('仍可查看的整理结果')), ...o.verified.map(knownRow)] : null].flat()
    : o.rows.map(rowEl);
  return [h('section', { class: 'sheet df-sheet fb-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('批量格式整理'), 'data-phase': o.phase },
    ribbon(), h('header', { class: 'ce-head' },
      h('button', { class: 'icon-btn', 'aria-label': tx('关闭'), onclick: close }, icon('x', 20)),
      h('div', { class: 'ce-title' }, h('div', { class: 'ce-what' }, tx('批量格式整理')),
        h('div', { class: 'ce-sub' }, tx('分组：{0}', o.groupName || ''), o.sel.include_descendants ? ` · ${tx('含子文件夹')}` : ''))),
    top, usage, o.rowsErr && o.phase !== 'denied' ? h('div', { class: 'df-count' }, tx('结果没有读取完：{0}', fmtErr(o.rowsErr))) : null,
    h('div', { class: 'scroll fb-list' }, list,
      o.phase !== 'denied' && o.next != null ? h('button', { class: 'btn fr-more', disabled: !!o.loadingRows, onclick: () => readResults(o, o.next) }, tx('继续载入')) : null,
      ['done', 'paused', 'rejected'].includes(o.phase) || (o.phase === 'denied') ? h('button', { class: 'btn fr-more', onclick: () => { if (o.phase === 'denied' || confirm(tx('重新整理会先重新预览并确认，之前的结果仍保留。继续吗？'))) { localStorage.removeItem(fmtReqKey(o.sel)); state.fmtBatch = null; askBatch(); } } }, tx('重新整理这个分组')) : null))];
}

function bookPicker(it) {
  const b = it.book;
  if (!b?.doc || !b.picker) return [];
  const nav = bookNav(b, state.lang); const idx = bookIndex(b, nav, state.lang);
  const close = () => { b.picker = false; render(); };
  const sub = state.lang === 'orig' ? tx('原文目录') : state.lang === 'trans' ? tx('译文目录') : tx('对照目录');
  requestAnimationFrame(() => document.querySelector('.book-toc.on')?.scrollIntoView({ block: 'center' }));
  return [h('div', { class: 'scrim', onclick: close }),
    h('div', { class: 'sheet book-picker', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('章节') },
      h('div', { class: 'grab' }, h('span')),
      h('h2', {}, tx('章节'), h('span', { class: 'book-sub' }, sub)),
      h('div', { class: 'body' }, nav.map((n, i) => {
        const only = state.lang === 'bi' && !n.c.counterpart ? (n.side === 'original' ? tx('仅原文') : tx('仅译文')) : null;
        const row = h('button', { class: `book-toc${i === idx ? ' on' : ''}`, 'aria-current': i === idx ? 'true' : null, 'data-key': n.c.key, 'data-side': n.side, onclick: () => bookGo(it, n) },
          h('span', { class: 'grow' }, n.c.title), only ? h('span', { class: 'tag' }, only) : null);
        row.style.paddingLeft = `${12 + Math.max(0, Math.min(4, n.c.level - 1)) * 18}px`; // level indent
        return row;
      })))];
}

// Markdown blocks exactly as the agent splits them (memex-agent markdown_blocks::split_blocks, the
// same port the desktop reader uses): blank lines separate blocks, a fenced code block is one
// block and an unclosed fence runs to the end. The agent's translation keeps the original's block
// count and order (passthrough blocks byte-identical), so 对照 pairs block i with block i.
function fenceMarker(line) {
  const indent = line.length - line.replace(/^ +/, '').length;
  if (indent > 3) return null;
  const rest = line.slice(indent);
  const m = rest[0];
  if (m !== '`' && m !== '~') return null;
  let n = 0; while (rest[n] === m) n += 1;
  return n >= 3 ? { m, n, indent } : null;
}
const closesFence = (line, m, n) => { const f = fenceMarker(line); return !!f && f.m === m && f.n >= n && line.slice(f.indent + f.n).trim() === ''; };
const isImageLine = (l) => { l = l.trim(); return (l.startsWith('![') || l.startsWith('[![')) && l.endsWith(')') && l.includes(']('); };
const isUrlLine = (l) => { l = l.trim(); if (l.startsWith('<') && l.endsWith('>')) l = l.slice(1, -1); return /^https?:\/\//.test(l) && !/\s/.test(l); };
const isTableSep = (l) => { l = l.trim(); return l.includes('-') && l.includes('|') && /^[|\-: \t]*$/.test(l); };
function passthrough(text) {
  const lines = text.split('\n').map((l) => l.replace(/\r$/, '')).filter((l) => l.trim() !== '');
  if (!lines.length) return true;
  const table = lines.every((l) => l.trimStart().startsWith('|')) || (lines.length >= 2 && isTableSep(lines[1]));
  return table || lines.every((l) => isImageLine(l) || isUrlLine(l));
}
function splitBlocks(md) {
  const blocks = []; let cur = null; let fence = null; let offset = 0;
  // `start`: the block's first character in `md` (the reader's source map for 手工标记 and 阅读位置).
  const flush = () => { if (cur) { const text = md.slice(cur[0], cur[1]); blocks.push({ text, passthrough: passthrough(text), fenced: false, start: cur[0] }); cur = null; } };
  for (const raw of md.match(/[^\n]*\n|[^\n]+$/g) || []) {
    const start = offset; offset += raw.length;
    const line = raw.endsWith('\n') ? raw.slice(0, -1) : raw;
    const end = start + line.length - (line.endsWith('\r') ? 1 : 0);
    const content = md.slice(start, end);
    if (fence) {
      if (closesFence(content, fence.m, fence.n)) { blocks.push({ text: md.slice(fence.start, end), passthrough: true, fenced: true, start: fence.start }); fence = null; }
      continue;
    }
    const f = fenceMarker(content);
    if (f) { flush(); fence = { start, m: f.m, n: f.n }; continue; }
    if (content.trim() === '') { flush(); continue; }
    if (cur) cur[1] = end; else cur = [start, end];
  }
  if (fence) blocks.push({ text: md.slice(fence.start).replace(/[\n\r]+$/, ''), passthrough: true, fenced: true, start: fence.start });
  flush();
  return blocks;
}
// Rows like the desktop's bilingual::pair: null when the counts differ (never mis-paired).
function pairBlocks(original, translation) {
  if (original.length !== translation.length) return null;
  return original.map((o, i) => (o.text === translation[i].text || (o.passthrough && translation[i].passthrough) ? { shared: o } : { o, t: translation[i] }));
}
// Blocks safe to render while more text is still loading: a block cut by the segment boundary
// (the last one, including an unclosed fence) waits for the next segment.
const completeBlocks = (text, more) => { const bs = splitBlocks(text); return more && bs.length ? bs.slice(0, -1) : bs; };

// ---------- Markdown → DOM (no innerHTML; text nodes and a fixed set of elements) ----------
// Block constructs inside one split block: headings, paragraphs, (nested) lists, blockquotes,
// tables (scroll inside their own box), rules. Inline: strong, emphasis, strikethrough, code,
// links (http/https/mailto only, rel=noopener noreferrer), images as a labelled link (never
// loaded), escapes; raw HTML stays text. Every text run is a verbatim slice of the raw block, so
// 重点标注 spans (offsets into the raw block) wrap the right characters after rendering.
const SAFE_HREF = /^(https?:\/\/|mailto:)[^\s"'<>]+$/i;
const LOCAL_IMAGE = /^attachments\/(?:sha256-)?[a-f0-9]{64}\.(png|jpe?g|gif|webp)$/;
let stopImages = () => {};
// One visible reader at a time. No persistent image cache, no external image
// request. Cancel late chunks and revoke object URLs when leaving/re-rendering.
function bindImages(content, scroller, it) {
  stopImages();
  if (!source?.imageRead || !it?.meta?.revision_id || it.cache === 'offline') return;
  const abort = new AbortController(); const urls = new Map(); const visible = new Set(); const pending = new Set(); const queue = []; let active = 0;
  const valid = () => !abort.signal.aborted && state.item === it && content.isConnected;
  const pump = () => {
    while (active < 2 && queue.length && !abort.signal.aborted) {
      const el = queue.shift(); active += 1;
      (async () => {
        if (!valid() || !visible.has(el)) return;
        try {
          const blob = await source.imageRead({ item_id: it.id, expected_revision_id: it.meta.revision_id, target: el.dataset.imageTarget, signal: abort.signal });
          if (!valid() || !visible.has(el)) return;
          const url = URL.createObjectURL(blob); urls.set(el, url);
          const img = h('img', { src: url, alt: el.dataset.imageAlt || tx('正文配图'), decoding: 'async' });
          img.onload = () => {
            if (img.naturalWidth * img.naturalHeight > 16_000_000) { el.textContent = tx('图片尺寸过大，暂不显示'); URL.revokeObjectURL(url); urls.delete(el); }
          };
          img.onerror = () => { el.textContent = tx('图片格式无法解码'); URL.revokeObjectURL(url); urls.delete(el); };
          el.replaceChildren(img); el.classList.add('loaded');
        } catch {
          if (valid()) el.textContent = tx('图片不可用：附件缺失、无权限或版本已变化');
        }
      })().finally(() => { pending.delete(el); active -= 1; pump(); });
    }
  };
  const observer = new IntersectionObserver(entries => {
    for (const e of entries) {
      const el = e.target;
      if (e.isIntersecting) {
        visible.add(el);
        if (!urls.has(el) && !pending.has(el)) { pending.add(el); queue.push(el); }
      } else {
        visible.delete(el);
        if (urls.has(el)) {
          // Keep the measured height so releasing an offscreen image does not
          // jump the scroll position. A later intersection loads it again.
          el.style.minHeight = `${rectIn(el).height}px`;
          URL.revokeObjectURL(urls.get(el)); urls.delete(el);
          el.textContent = tx('正在载入本地图片…');
        }
      }
    }
    pump();
  }, { root: scroller, rootMargin: '200px' });
  for (const el of content.querySelectorAll('[data-image-target]')) observer.observe(el);
  stopImages = () => { abort.abort(); observer.disconnect(); queue.length = 0; for (const url of urls.values()) URL.revokeObjectURL(url); urls.clear(); };
}

// ---------- 条目封面 (item_cover_v1) ----------
// A cover is its own resource beside the item, never part of the body: one block above the article
// content, outside every [data-b] block, so the source map, UTF-8 coordinates, marks, selections,
// 阅读记录 and speaker overlays see the same body with or without it. Nothing is requested unless
// session.info.item_cover_v1 is 'available'; a null batch position is "no cover" (no placeholder).
// The reader reads the original (thumbnail=false) until next_offset=null and shows it only when
// total_bytes and the SHA-256 match; stale_cover_version → one new batch, then from offset 0 again
// (pieces of two versions are never joined). Not cached; the object URL lives while the article shows.
const coverOn = () => state.caps?.item_cover_v1 === 'available';
async function coverBatch(ids) {
  const out = [];
  for (let i = 0; i < ids.length; i += 50) {
    const part = ids.slice(i, i + 50); const r = await source.itemCoverBatch({ item_ids: part });
    if (!Array.isArray(r) || r.length !== part.length || r.some((c, k) => c !== null && c?.item_id !== part[k])) throw codeErr('invalid_response');
    out.push(...r);
  }
  return out;
}
// One raster (original or thumbnail) of exactly `cover.version`, verified → Blob; any mismatch throws.
async function coverBytes(cover, thumbnail, signal) {
  const d = thumbnail ? cover.thumbnail : cover.raster;
  const parts = []; let offset = 0;
  for (;;) {
    if (signal?.aborted) throw codeErr('cancelled');
    const r = await source.itemCoverChunk({ item_id: cover.item_id, version: cover.version, thumbnail, offset });
    if (signal?.aborted) throw codeErr('cancelled');
    if (r.offset !== offset || r.media_type !== d.media_type || r.sha256 !== d.sha256 || r.total_bytes !== d.byte_size) throw codeErr('cover_mismatch');
    let bytes;
    try { bytes = Uint8Array.from(atob(r.data_base64), (c) => c.charCodeAt(0)); } catch { throw codeErr('invalid_response'); }
    if (!bytes.length || bytes.length > 96 * 1024 || offset + bytes.length > d.byte_size) throw codeErr('cover_mismatch');
    parts.push(bytes); offset += bytes.length;
    if (r.next_offset !== (offset < d.byte_size ? offset : null)) throw codeErr('cover_mismatch');
    if (r.next_offset === null) break;
  }
  const all = new Uint8Array(d.byte_size); let at = 0;
  for (const p of parts) { all.set(p, at); at += p.length; }
  if (await sha256Hex(all) !== d.sha256) throw codeErr('cover_mismatch');
  return new Blob([all], { type: d.media_type });
}
// Batch, then the raster; a version that moves on mid-read restarts once from a new batch.
async function readCover(cover, item_id, thumbnail, signal) {
  for (let round = 0; ; round += 1) {
    if (!cover) return null;
    try { return { cover, blob: await coverBytes(cover, thumbnail, signal) }; } catch (e) {
      if (e.code !== 'stale_cover_version' || round >= 1) throw e;
    }
    [cover] = await coverBatch([item_id]);
  }
}
const readItemCover = async (item_id, signal) => (coverOn() ? readCover((await coverBatch([item_id]))[0], item_id, false, signal) : null);
// For a future tiled view (YouTube-style episode covers): thumbnails only, ≤ 50 ids per batch, never
// the original. Map item_id → { cover, blob }; null positions and failed reads are simply absent.
async function readCoverThumbnails(ids, { signal } = {}) {
  const out = new Map();
  if (!coverOn() || !ids.length) return out;
  const unique = [...new Set(ids)];
  const covers = await coverBatch(unique);
  for (let i = 0; i < unique.length; i += 1) {
    if (signal?.aborted) break;
    try { const got = await readCover(covers[i], unique[i], true, signal); if (got) out.set(unique[i], got); } catch { /* this tile stays without a cover */ }
  }
  return out;
}
// The open reader's cover (one at a time): { it, state: loading|ready|none, url, w, h, abort }.
let coverView = null;
function dropCover() {
  if (!coverView) return;
  coverView.abort.abort();
  if (coverView.url) URL.revokeObjectURL(coverView.url);
  coverView = null;
}
function coverFor(it) {
  if (coverView?.it === it) return coverView;
  dropCover();
  if (!it?.meta || it.book) return null;
  const view = coverView = { it, state: 'loading', url: null, w: 0, h: 0, abort: new AbortController() };
  (async () => {
    let url = null;
    try {
      try { await caps(); } catch { /* no session.info: no cover request */ }
      if (coverView !== view) return;
      const got = await readItemCover(it.id, view.abort.signal);
      if (coverView !== view || !got) { view.state = 'none'; return; }
      url = URL.createObjectURL(got.blob);
      const img = new Image(); img.src = url;
      await img.decode(); // a raster the browser cannot decode is never shown
      if (coverView !== view) { URL.revokeObjectURL(url); return; }
      Object.assign(view, { state: 'ready', url, w: img.naturalWidth, h: img.naturalHeight });
      placeCover(view);
    } catch {
      if (url && view.url !== url) URL.revokeObjectURL(url);
      view.state = 'none';
    }
  })();
  return view;
}
const coverShown = (it) => !vertNow(it) && state.insView === 'text';
function coverEl(it) {
  const v = coverFor(it);
  if (v?.state !== 'ready' || !coverShown(it)) return null;
  return h('figure', { class: 'item-cover', 'data-cover': it.id }, h('img', { src: v.url, alt: tx('条目封面'), width: String(v.w), height: String(v.h), draggable: 'false' }));
}
// The cover arrived after the article was drawn: put it on top without moving what the reader sees.
function placeCover(view) {
  const sc = document.getElementById('reader-scroll');
  const content = sc?.querySelector(':scope > .article > div');
  if (state.item !== view.it || sc?.dataset.item !== view.it.id || !content || content.querySelector(':scope > .item-cover') || !coverShown(view.it)) return;
  const first = content.firstElementChild; const y0 = first?.getBoundingClientRect().top;
  content.prepend(coverEl(view.it));
  if (first && sc.scrollTop > 0) sc.scrollTop += first.getBoundingClientRect().top - y0;
  chromeKey = null; requestAnimationFrame(updateReaderChrome);
}
function mdLink(url, kids, cls = '') {
  const u = url.trim().replace(/^<(.*)>$/, '$1');
  return SAFE_HREF.test(u) ? h('a', { href: u, target: '_blank', rel: 'noopener noreferrer', class: cls || null }, kids)
    : h('span', { class: `md-refused ${cls}`.trim(), title: tx('链接不安全，已停用') }, kids);
}
// Text [s, e) of the raw block, split by highlight marks ({start, end, n, style, label}).
// 简繁 (ctx.zh) converts each piece for display only: marks keep their original byte ranges.
// 竖排 (ctx.tcy) sets one- and two-digit numbers upright in one cell (纵中横), e.g. 「第 01 章」.
const TCY = /(?<![0-9A-Za-z])[0-9]{1,2}(?![0-9A-Za-z])/g;
// Source map: every text node cut verbatim from a reader block records where it starts in that
// block's raw text (`SRC`), so a DOM selection resolves to exact source characters (never a search
// for the first occurrence of the selected words). Converted 简繁 text has no map.
const SRC = new WeakMap();
// A renamed speaker label's text node → the raw name range {s, e}: it maps only as a whole (its start
// to s, its end to e); a boundary inside it maps nowhere (never a proportional byte guess).
const SPKL = new WeakMap();
function mdText(raw, s, e, parent, ctx) {
  let p = s; const T = ctx.zh || ((x) => x);
  const node = (str, at) => { const n = document.createTextNode(str); if (!ctx.zh && ctx.srcMap) SRC.set(n, at); return n; };
  const put = (el, str, at) => {
    if (!ctx.tcy) { el.append(node(str, at)); return; }
    let last = 0;
    for (const m of str.matchAll(TCY)) { if (m.index > last) el.append(node(str.slice(last, m.index), at + last)); el.append(h('span', { class: 'tcy' }, node(m[0], at + m.index))); last = m.index + m[0].length; }
    if (last < str.length) el.append(node(str.slice(last), at + last));
  };
  // 说话人名字 (ctx.swap): the label's name [s, e) is shown as another name in an unmapped span, so no
  // selection resolves into it and every other character keeps its source position.
  const run = (el, from, to) => {
    const w = ctx.swap;
    if (!w || to <= w.s || from >= w.e) { if (to > from) put(el, T(raw.slice(from, to)), from); return; }
    if (w.s > from) put(el, T(raw.slice(from, w.s)), from);
    if (w.s >= from) {
      const tn = document.createTextNode(w.text); if (ctx.srcMap) SPKL.set(tn, { s: w.s, e: w.e });
      el.append(h('span', { class: 'spk-name', 'data-spk-raw': raw.slice(w.s, w.e) }, tn));
    }
    if (to > w.e) put(el, T(raw.slice(w.e, to)), w.e);
  };
  for (const m of ctx.marks || []) {
    if (m.end <= p || m.start >= e) continue;
    const a = Math.max(m.start, p); const b = Math.min(m.end, e);
    if (a > p) run(parent, p, a);
    const mk = markEl(m, ctx); run(mk, a, b); parent.append(mk);
    p = b;
  }
  if (e > p) run(parent, p, e);
}
// One mark piece: a 重点标注 (AI, `n` set), a 手工标记 piece (`um`: the marks covering it), or both.
function markEl(m, ctx) {
  if (!m.um) return h('mark', { class: `hl ${m.style}`, 'data-hl': String(m.n), 'data-side': ctx.side || 'o', title: m.label || null });
  const cls = [m.n != null ? `hl ${m.style}` : '', 'um', ...umClasses(m.um)].filter(Boolean).join(' ');
  // data-um: the mark on top here (a tap edits it); data-ums: every manual mark covering this piece.
  return h('mark', { class: cls, 'data-hl': m.n != null ? String(m.n) : null, 'data-um': m.um[0].id, 'data-ums': m.um.map((x) => x.id).join(' '), 'data-side': ctx.side || 'o', title: m.n != null ? m.label || null : null });
}
// `[text](url)` starting at the bracket content `s`: {close, urlStart, urlEnd, end} or null.
function mdBracket(raw, s, to) {
  let depth = 0;
  for (let j = s; j < to; j += 1) {
    const c = raw[j];
    if (c === '\\') { j += 1; continue; }
    if (c === '\n' && raw[j + 1] === '\n') return null;
    if (c === '[') depth += 1;
    else if (c === ']') {
      if (depth) { depth -= 1; continue; }
      if (raw[j + 1] !== '(') return null;
      let par = 0;
      for (let k = j + 2; k < to; k += 1) {
        if (raw[k] === '\n' || raw[k] === ' ' && raw[k + 1] !== '"') return null;
        if (raw[k] === '(') par += 1;
        else if (raw[k] === ')') { if (par) par -= 1; else return { close: j, urlStart: j + 2, urlEnd: k, end: k + 1 }; }
      }
      return null;
    }
  }
  return null;
}
const isWordChar = (c) => !!c && /[\p{L}\p{N}]/u.test(c);
function mdInline(raw, from, to, parent, ctx) {
  let i = from; let plain = from;
  const flush = (end) => { if (end > plain) mdText(raw, plain, end, parent, ctx); };
  while (i < to) {
    const c = raw[i];
    if (c === '\\' && i + 1 < to && /[!-/:-@[-`{-~]/.test(raw[i + 1])) { flush(i); mdText(raw, i + 1, i + 2, parent, ctx); i += 2; plain = i; continue; }
    if (c === '\n') { flush(i); parent.append(ctx.breaks ? h('br') : '\n'); i += 1; plain = i; continue; }
    if (c === '`') {
      let n = 1; while (raw[i + n] === '`') n += 1;
      let close = raw.indexOf('`'.repeat(n), i + n);
      while (close >= 0 && close < to && raw[close + n] === '`') close = raw.indexOf('`'.repeat(n), close + n + 1);
      if (close > 0 && close + n <= to) {
        flush(i);
        let a = i + n; let b = close;
        if (b - a >= 2 && raw[a] === ' ' && raw[b - 1] === ' ') { a += 1; b -= 1; }
        const code = h('code'); mdText(raw, a, b, code, ctx); parent.append(code);
        i = close + n; plain = i; continue;
      }
      i += n; continue;
    }
    if ((c === '!' && raw[i + 1] === '[') || c === '[') {
      const s = c === '!' ? i + 2 : i + 1;
      const m = mdBracket(raw, s, to);
      if (m) {
        flush(i);
        const url = raw.slice(m.urlStart, m.urlEnd).replace(/\s+"[^"]*"$/, '');
        if (c === '!') {
          const alt = raw.slice(s, m.close).trim();
          parent.append(ctx.readerImage && LOCAL_IMAGE.test(url)
            ? h('span', { class: 'md-local-image', 'data-image-target': url, 'data-image-alt': alt, role: 'img', 'aria-label': alt || tx('正文配图') }, tx('正在载入本地图片…'))
            : mdLink(url, [icon('doc', 13), alt ? tx('图片：{0}', alt) : tx('图片')], 'md-img'));
        } else {
          const label = h('span'); mdInline(raw, s, m.close, label, ctx);
          parent.append(mdLink(url, [...label.childNodes]));
        }
        i = m.end; plain = i; continue;
      }
    }
    if (c === '<' && /^<font\b/i.test(raw.slice(i, i + 5))) {
      // <font color=red>…</font> (古籍 editions mark text this way): red becomes the warm red; any
      // other colour, or none, keeps the text plain. Never HTML: the tags are dropped, the text drawn.
      const open = /^<font(?:\s+color\s*=\s*(?:"([^"<>]*)"|'([^'<>]*)'|([^\s"'<>]+)))?\s*>/i.exec(raw.slice(i, to));
      const close = open ? raw.slice(i + open[0].length, to).search(/<\/font\s*>/i) : -1;
      if (open && close >= 0) {
        flush(i);
        const color = (open[1] ?? open[2] ?? open[3] ?? '').trim().toLowerCase();
        const el = h('span', { class: ['red', '#f00', '#ff0000', 'crimson', 'darkred'].includes(color) ? 'md-red' : 'md-font' });
        const a = i + open[0].length; mdInline(raw, a, a + close, el, ctx); parent.append(el);
        i = a + close + /^<\/font\s*>/i.exec(raw.slice(a + close, to))[0].length; plain = i; continue;
      }
    }
    if (c === '<') {
      const m = /^<((?:https?:\/\/|mailto:)[^\s<>]+)>/i.exec(raw.slice(i, to));
      if (m) { flush(i); const a = h('span'); mdText(raw, i + 1, i + 1 + m[1].length, a, ctx); parent.append(mdLink(m[1], [...a.childNodes])); i += m[0].length; plain = i; continue; }
    }
    if (c === 'h' && (i === from || !/[A-Za-z0-9/]/.test(raw[i - 1]))) {
      const m = /^https?:\/\/[^\s<>()（）「」，。]+/.exec(raw.slice(i, to));
      if (m) {
        const url = m[0].replace(/[.,;:!?'"）)]+$/, '');
        flush(i); const a = h('span'); mdText(raw, i, i + url.length, a, ctx); parent.append(mdLink(url, [...a.childNodes]));
        i += url.length; plain = i; continue;
      }
    }
    if (c === '*' || c === '_' || c === '~') {
      let n = 1; while (raw[i + n] === c) n += 1;
      const tries = c === '~' ? (n >= 2 ? [2] : []) : n >= 2 ? [2, 1] : [1];
      let done = false;
      for (const k of tries) {
        const cs = i + k;
        if (cs >= to || /\s/.test(raw[cs]) || (c === '_' && isWordChar(raw[i - 1]))) continue;
        let j = raw.indexOf(c.repeat(k), cs + 1);
        while (j >= 0 && j + k <= to && (/\s/.test(raw[j - 1]) || (k === 1 && raw[j + 1] === c) || (c === '_' && isWordChar(raw[j + k])))) j = raw.indexOf(c.repeat(k), j + (k === 1 && raw[j + 1] === c ? 2 : 1));
        if (j < 0 || j + k > to) continue;
        flush(i);
        const el = h(c === '~' ? 'del' : k === 2 ? 'strong' : 'em');
        mdInline(raw, cs, j, el, ctx); parent.append(el);
        i = j + k; plain = i; done = true; break;
      }
      if (!done) i += n;
      continue;
    }
    i += 1;
  }
  flush(to);
}
const MD_HEAD = /^ {0,3}(#{1,6})(?:[ \t]+|$)/;
const MD_HR = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const MD_LIST = /^([ \t]*)(?:[-*+]|(\d{1,9})[.)])(?:[ \t]+|$)/;
const MD_QUOTE = /^ {0,3}> ?/;
// One split block → its elements. `ctx`: { marks, side, breaks }.
function mdBlock(raw, ctx = {}) {
  const lines = []; let off = 0;
  for (const l of raw.split('\n')) { const t = l.replace(/\r$/, ''); lines.push({ s: off, e: off + t.length, t }); off += l.length + 1; }
  // Hide the fallback control line without changing block indexes or source coordinates.
  if (lines[0]?.t === '<!-- memex:untranslated -->') lines.shift();
  const out = [];
  const tableAt = (k) => !!lines[k + 1] && lines[k].t.includes('|') && isTableSep(lines[k + 1].t);
  const starts = (k) => MD_HEAD.test(lines[k].t) || MD_HR.test(lines[k].t) || MD_QUOTE.test(lines[k].t) || MD_LIST.test(lines[k].t) || tableAt(k);
  const lineInto = (el, L, from) => mdInline(raw, from, L.e, el, ctx);
  let i = 0;
  while (i < lines.length) {
    const L = lines[i];
    if (!L.t.trim()) { i += 1; continue; }
    let m;
    if ((m = MD_HEAD.exec(L.t))) {
      const level = m[1].length;
      const el = h(level <= 1 ? 'h2' : level === 2 ? 'h3' : 'h4');
      const end = L.s + L.t.replace(/[ \t]+#+[ \t]*$/, '').replace(/[ \t]+$/, '').length;
      mdInline(raw, L.s + m[0].length, Math.max(end, L.s + m[0].length), el, ctx);
      out.push(el); i += 1; continue;
    }
    if (MD_HR.test(L.t)) { out.push(h('hr')); i += 1; continue; }
    if (tableAt(i)) {
      const rows = [];
      while (i < lines.length && lines[i].t.includes('|') && lines[i].t.trim()) rows.push(lines[i++]);
      const cells = (R) => {
        const r = []; let st = R.s;
        for (let k = R.s; k <= R.e; k += 1) if (k === R.e || (raw[k] === '|' && raw[k - 1] !== '\\')) { r.push([st, k]); st = k + 1; }
        if (raw.slice(r[0][0], r[0][1]).trim() === '' && R.t.trim().startsWith('|')) r.shift();
        if (r.length && raw.slice(r[r.length - 1][0], r[r.length - 1][1]).trim() === '' && R.t.trim().endsWith('|')) r.pop();
        return r.map(([a, b]) => { while (a < b && /\s/.test(raw[a])) a += 1; while (b > a && /\s/.test(raw[b - 1])) b -= 1; return [a, b]; });
      };
      const align = cells(rows[1]).map(([a, b]) => { const c = raw.slice(a, b); return c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : null; });
      const row = (R, tag) => h('tr', {}, cells(R).map(([a, b], k) => { const td = h(tag); if (align[k]) td.style.textAlign = align[k]; mdInline(raw, a, b, td, ctx); return td; }));
      out.push(h('div', { class: 'table-wrap' }, h('table', {}, h('thead', {}, row(rows[0], 'th')), h('tbody', {}, rows.slice(2).map((R) => row(R, 'td'))))));
      continue;
    }
    if (MD_QUOTE.test(L.t)) {
      const q = h('blockquote'); let p = null;
      while (i < lines.length && MD_QUOTE.test(lines[i].t)) {
        const Q = lines[i++]; const pre = MD_QUOTE.exec(Q.t)[0].length;
        if (!Q.t.slice(pre).trim()) { p = null; continue; }
        if (p) p.append(ctx.breaks ? h('br') : '\n'); else { p = h('p'); q.append(p); }
        lineInto(p, Q, Q.s + pre);
      }
      out.push(q); continue;
    }
    if (MD_LIST.test(L.t)) {
      const stack = [];
      while (i < lines.length && lines[i].t.trim()) {
        const R = lines[i]; const lm = MD_LIST.exec(R.t);
        if (!lm) {
          if (stack.length && !(MD_HEAD.test(R.t) || MD_HR.test(R.t) || MD_QUOTE.test(R.t) || tableAt(i))) {
            const li = stack[stack.length - 1].li; li.append(ctx.breaks ? h('br') : '\n');
            lineInto(li, R, R.s + (R.t.length - R.t.trimStart().length)); i += 1; continue;
          }
          break;
        }
        const ind = lm[1].replace(/\t/g, '    ').length; const ordered = lm[2] != null;
        while (stack.length && stack[stack.length - 1].ind > ind) stack.pop();
        let top = stack[stack.length - 1];
        if (top && top.ind === ind && top.ordered !== ordered) { stack.pop(); top = stack[stack.length - 1]; }
        if (!top || ind > top.ind) {
          const list = h(ordered ? 'ol' : 'ul', { start: ordered && lm[2] !== '1' ? String(Number(lm[2])) : null });
          if (top) top.li.append(list); else out.push(list);
          top = { ind, ordered, list, li: null }; stack.push(top);
        }
        const li = h('li');
        let from = R.s + lm[0].length;
        const task = /^\[([ xX])\][ \t]+/.exec(raw.slice(from, R.e));
        if (task) { li.append(h('span', { class: 'md-task', 'aria-hidden': 'true' }, task[1] === ' ' ? '☐ ' : '☑ ')); from += task[0].length; }
        mdInline(raw, from, R.e, li, ctx);
        top.list.append(li); top.li = li; i += 1;
      }
      continue;
    }
    const p = h('p');
    lineInto(p, L, L.s); i += 1;
    while (i < lines.length && lines[i].t.trim() && !starts(i)) { p.append(ctx.breaks ? h('br') : '\n'); lineInto(p, lines[i], lines[i].s); i += 1; }
    out.push(p);
  }
  return out;
}
const fenceBody = (text) => text.split('\n').slice(1, /^\s*(```|~~~)/.test(text.split('\n').pop()) ? -1 : undefined).join('\n');
// A whole Markdown text (minutes, chat replies): its split blocks rendered in order.
function mdDoc(md, ctx = {}) {
  return h('div', { class: 'md' }, splitBlocks(md || '').flatMap((b) => (b.fenced ? [h('pre', { class: 'code' }, fenceBody(b.text))] : mdBlock(b.text, ctx))));
}
// One reader block (index `idx` of split_blocks on side 'o' original or 't' translation).
function blockEl(b, cls = '', idx = null, side = 'o') {
  const at = idx == null ? {} : { [side === 't' ? 'data-bt' : 'data-b']: String(idx) };
  if (b.fenced) return h('pre', { class: `code ${cls}`.trim(), ...at }, fenceBody(b.text));
  const plain = side === 'o' && idx != null && b.start != null && state.item && !state.item.book;
  let swap = null;
  if ((plain || (side === 't' && idx != null && state.item && !state.item.book)) && !b.fenced) {
    const sl = speakerLabel(state.item, side, b.text);
    if (sl) { at['data-spk'] = sl.show; if (!sl.ref) at['data-spk-ref'] = 'none'; if (sl.show !== b.text.slice(sl.s, sl.e)) swap = { s: sl.s, e: sl.e, text: sl.show }; }
  }
  const ai = idx == null ? [] : marksFor(idx, side, b.text);
  const parts = mdBlock(b.text, { marks: plain ? mergeMarks(ai, manualFor(state.item, b, b.text)) : ai, side, readerImage: true, zh: curZh(), tcy: vertNow(state.item), srcMap: plain, swap });
  if (isNoteText(b.text)) cls = `${cls} lnote`.trim();
  if (parts.length === 1 && !parts[0].classList.contains('table-wrap')) {
    if (cls) parts[0].classList.add(...cls.split(' '));
    for (const [k, v] of Object.entries(at)) parts[0].setAttribute(k, v);
    return parts[0];
  }
  return h('div', { class: `mdb ${cls}`.trim(), ...at }, parts);
}
// UTF-8 byte offset → string index in `s` (-1 if it is not a character boundary).
function byteIdx(s, b) {
  let bytes = 0;
  for (let i = 0; i <= s.length;) {
    if (bytes === b) return i;
    if (i === s.length || bytes > b) return -1;
    const cp = s.codePointAt(i);
    bytes += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4; i += cp > 0xffff ? 2 : 1;
  }
  return -1;
}
// 重点标注 for block `idx` on one side. v2 items carry exact spans (UTF-8 offsets into the raw
// block) per side; a v1 item (no spans) is a quote found verbatim in the original. Spans that
// do not match the text, or overlap one already placed, are skipped, never guessed.
const HL_STYLES = ['bold', 'highlight', 'positive', 'negative'];
function marksFor(idx, side, raw, items = null) {
  if (!items && (!state.emphasis || state.insView !== 'text')) return [];
  const out = [];
  (items || insightDone('highlights') || []).forEach((x, n) => {
    if (x.block_index !== idx) return;
    const sp = side === 't' ? x.translation : x.original;
    let start = -1; let end = -1;
    if (sp && sp.block === idx) {
      const a = byteIdx(raw, sp.start); const b = byteIdx(raw, sp.end);
      if (a >= 0 && b > a && raw.slice(a, b) === sp.text) { start = a; end = b; }
    } else if (side === 'o' && !x.original && !x.translation && x.quote) {
      start = raw.indexOf(x.quote); end = start + x.quote.length;
    }
    if (start < 0 || out.some((m) => start < m.end && end > m.start)) return;
    out.push({ start, end, n, style: HL_STYLES.includes(x.style) ? x.style : 'other', label: x.label || null });
  });
  return out.sort((a, b) => a.start - b.start);
}

const REASON_TEXT = {
  invalid_reader_anchor: tx('选中的位置已失效，请重新选择'), reader_annotation_limit_exceeded: tx('这篇的标记已达上限'),
  reader_annotation_not_found: tx('这条标记已被删除'), reader_annotations_unavailable: tx('标记暂时读不出来'),
  source_is_chinese: tx('原文为中文，无需翻译'), no_translation_provider: tx('还没有配置翻译模型'), translation_unavailable: tx('还没有配置翻译模型'),
  provider_timeout: tx('模型服务响应超时'), translation_provider_timeout: tx('模型服务响应超时'), minutes_provider_timeout: tx('模型服务响应超时'),
  provider_failure: tx('模型服务返回错误'), translation_provider_failure: tx('模型服务返回错误'), minutes_provider_failure: tx('模型服务返回错误'),
  retry_exhausted: tx('多次重试后仍未成功'), malformed_model_output: tx('模型返回的格式不对'), stale_revision: tx('原文已修改，结果已过期'),
  capture_hook_pending: tx('等待排入翻译队列'), source_too_large: tx('原文过长，暂不支持'), challenge_or_login: tx('抓取到的是验证或登录页'),
  unsupported_item_type: tx('这类条目暂不支持'), no_minutes_provider: tx('还没有配置纪要模型'), no_summary_provider: tx('还没有配置纪要模型'),
  minutes_unavailable: tx('还没有配置纪要模型'), minutes_not_applicable: tx('正文太少，无法生成纪要'), permission_denied: tx('当前连接没有发起任务的权限'),
  unavailable: tx('无法连接到你的 Mac'), already_chinese: tx('原文为中文，无需翻译'),
  no_insight_provider: tx('还没有配置 AI 分析模型'), insight_translation_changed: tx('译文已更新，重点标注的位置已过期，需要重新生成'), insights_not_applicable: tx('正文太少，无法分析'),
  insights_unavailable: tx('结果已损坏或不可用'), invalid_insight_request: tx('请求格式不被这台 Mac 接受'),
  insights_too_large_for_read: tx('内容过多，手机上暂时无法显示，请在 Mac 上查看'),
  daemon_unavailable: tx('Mac 暂时没有响应'), library_resource_not_found: tx('条目不存在或无权访问'),
  invalid_request: tx('这台 Mac 的网关还不支持这项功能'), internal_error: tx('Mac 内部错误'),
  invalid_conversation_request: tx('请求格式不被这台 Mac 接受'), provider_outcome_unknown: tx('结果未知（可能已经调用了模型）'),
};
const reasonText = (r) => (r ? REASON_TEXT[r] || tx('代理原因：{0}', r) : null);
const frac = (p) => (p && p.total_batches ? `${p.done_batches}/${p.total_batches}` : null);
// [label, tone, action] for the item's translation; tone: ok | run | off | bad.
function translationLine(it) {
  const t = it.translation;
  if (it.trAsk === 'requesting') return [tx('正在请求翻译…'), 'run', null];
  const askErr = it.trAsk?.error;
  if (!t) return [tx('读取中…'), 'off', null];
  // A failed status read says why and offers a read again (never a request); a newer revision reopens.
  if (t.error === 'stale_item_revision') return [tx('条目已更新'), 'bad', [tx('重新打开'), () => loadItem(it.id)]];
  if (t.error) return [tx('读取翻译状态失败：{0}', ERR_TEXT[t.error] || REASON_TEXT[t.error] || t.error), 'bad', [tx('重新读取'), rereadTranslation]];
  const f = frac(t.progress);
  const retry = [tx('重试'), requestTranslation];
  const ask = [tx('翻译此文'), requestTranslation];
  const line = {
    succeeded: [tx('已翻译'), 'ok', null],
    queued: [f && t.progress.done_batches ? tx('排队中 {0}', f) : tx('排队中'), 'run', null],
    leased: [f ? tx('翻译中 {0}', f) : tx('翻译中'), 'run', null],
    running: [f ? tx('翻译中 {0}', f) : tx('翻译中'), 'run', null],
    awaiting_enqueue: [tx('等待排入翻译队列'), 'run', null],
    failed: [[f && t.progress.done_batches ? tx('翻译未完成（已完成 {0}）', f) : tx('翻译失败'), reasonText(t.reason)].filter(Boolean).join(' · '), 'bad', retry],
    skipped: [SKIP_TEXT[t.reason] || tx('这篇不翻译'), 'off', null],
    not_requested: t.reason === 'source_is_chinese' || looksChinese(it) ? [reasonText('source_is_chinese'), 'off', null] : [tx('未翻译'), 'off', ask],
    unavailable: [reasonText(t.reason) || tx('译文暂不可用'), 'off', t.reason ? null : [tx('重新读取'), rereadTranslation]],
  }[t.status] || [tx('译文暂不可用'), 'off', null];
  if (askErr) {
    line[0] = askErr === 'unknown' ? tx('请求结果未知，已向 Mac 读回状态（不会自动重发）')
      : askErr === 'invalid_request' ? tx('这台 Mac 的网关还不支持翻译请求') : tx('请求失败：{0}', reasonText(askErr));
  }
  return line;
}
const looksChinese = (it) => {
  const sample = it.text.slice(0, 4000);
  const cjk = (sample.match(/[\u3400-\u9fff]/g) || []).length;
  return cjk / ((sample.match(/[A-Za-z\u3400-\u9fff]/g) || []).length || 1) > 0.5;
};
function fallbackNote(it) {
  const t = it.translation;
  if (!t) return h('div', { class: 'note' }, tx('读取翻译状态…以下为原文。'));
  const busy = ['queued', 'running', 'leased', 'awaiting_enqueue'].includes(t.status);
  const f = frac(t.progress);
  const why = looksChinese(it) ? tx('原文为中文，无需翻译，以下为原文。')
    : t.error ? tx('译文暂不可用，以下为原文。')
      : t.status === 'skipped' ? `${SKIP_TEXT[t.reason] || tx('这篇不翻译。')}${tx('以下为原文。')}`
        : t.status === 'failed' ? tx('翻译失败，以下为原文。')
          : busy && f ? tx('正在翻译 {0}，完成前先显示原文。', f)
            : busy ? tx('正在翻译，完成前先显示原文。')
              : tx('还没有译文，以下为原文。');
  const [line, , action] = translationLine(it);
  // The request's own state (failure reason, unknown outcome, why the status could not be read) next
  // to the reason; progress is already in the reason line.
  const detail = ((busy && !f) || t.error || t.status === 'failed' || t.status === 'unavailable' || it.trAsk) && !looksChinese(it) ? line : null;
  return h('div', { class: 'note fallback-note' }, h('span', {}, why, detail ? h('span', { class: 'detail' }, detail) : null),
    action && !looksChinese(it) ? h('button', { class: 'btn-sm', onclick: action[1] }, action[0]) : null);
}
// 重新读取: the status again (read only, no request); the line shows 读取中… meanwhile.
function rereadTranslation() {
  const it = state.item;
  if (!it) return;
  it.translation = null; it.trAsk = null; softRender();
  loadTranslation();
}
// The reader toolbar's translate button, in 对照 and 译文 only. An ordinary item asks the Mac to
// translate it; a book chapter opens the existing 翻译本章 confirmation. Nothing is ever sent by itself.
function trControls(it) {
  if (state.lang === 'orig' || state.insView !== 'text') return [];
  const btn = (label, onclick, key, hint = null) => [h('button', { class: `btn-sm tr-open${onclick ? '' : ' off'}`, disabled: !onclick, 'data-tr': key, onclick }, label),
    hint ? h('span', { class: 'tr-hint', 'data-tr': key }, hint) : null];
  if (it.book) {
    const b = it.book; const L = tx('翻译本章');
    if (!b.doc) return [];
    const o = bookChapter(b, 'original'); const t = bookChapter(b, 'translation');
    if (t || !o) return btn(L, null, 'human', tx('这一章已有译本'));
    const ai = aiFor(it, b, o);
    if (!ai) return btn(L, null, 'unsupported', !(xlateOn() || xlateMetaOn()) ? tx('这台 Mac 的网关还不支持翻译请求') : xlateErr('reader_translation_language_unsupported'));
    const xp = it.xlate?.chapter === o.key ? it.xlate : null;
    if (xp?.phase === 'unknown') return btn(tx('查询原任务'), () => sendXlate(it, xp), 'unknown');
    if (xp && ACTIVE_XLATE.has(xp.phase)) {
      const j = xp.job; const n = Number.isSafeInteger(j?.total_blocks) && j.total_blocks > 0 && Number.isSafeInteger(j?.completed_blocks);
      return btn(n ? tx('翻译中 {0}', `${j.completed_blocks}/${j.total_blocks}`) : tx('翻译中'), null, 'running');
    }
    if (ai.state === 'ready') return btn(tx('已翻译'), null, 'done');
    if (it.xlate && ACTIVE_XLATE.has(it.xlate.phase)) return btn(L, null, 'other', tx('这本书的另一章正在翻译'));
    if (it.xlateAsk) return btn(L, null, 'asking');
    if (['eligible', 'none'].includes(ai.state) || (xp?.phase === 'failed' && !xp.committed)) {
      if (!xlateOn()) return btn(L, null, 'unsupported', tx('这台 Mac 的网关还不支持翻译请求'));
      if (!xlateEdit()) return btn(L, null, 'noedit', xlateErr('permission_denied'));
      if (ai.state === 'none') return btn(L, null, 'ineligible', XLATE_TELL.has(ai.reason) ? xlateErr(ai.reason) : null);
      return xp?.phase === 'failed' ? btn(tx('再试一次'), () => askXlate(it, o.key), 'failed') : btn(L, () => askXlate(it, o.key), 'eligible');
    }
    return btn(L, null, ai.state === 'error' ? 'error' : 'reading', ai.state === 'error' ? null : tx('读取中…'));
  }
  const t = it.translation; const L = tx('翻译此文');
  if (it.text && looksChinese(it)) return btn(L, null, 'chinese', reasonText('source_is_chinese'));
  if (it.trAsk === 'requesting') return btn(tx('正在请求翻译…'), null, 'requesting');
  if (!t) return btn(L, null, 'reading', tx('读取中…'));
  if (t.error) return btn(tx('重试'), () => trAfterReread(it), 'error');
  if (BUSY.has(t.status)) { const f = frac(t.progress); return btn(f ? tx('翻译中 {0}', f) : tx('翻译中'), null, 'running'); }
  if (t.status === 'succeeded') return btn(tx('已翻译'), null, 'done');
  if (t.status === 'skipped') return btn(L, null, 'skipped', SKIP_TEXT[t.reason] || tx('这篇不翻译'));
  if (t.status === 'not_requested' && t.reason === 'source_is_chinese') return btn(L, null, 'chinese', reasonText('source_is_chinese'));
  if (t.status === 'failed') return btn(tx('重试'), requestTranslation, 'failed');
  if (['not_requested', 'unavailable'].includes(t.status)) return btn(L, requestTranslation, t.status);
  return btn(L, null, 'other');
}
// 重试 after a failed status read: read the status first; only a job that is not there (not
// requested / failed / unavailable) is requested. Running or done: nothing is sent.
async function trAfterReread(it) {
  if (it.translation?.error === 'stale_item_revision') { loadItem(it.id); return; }
  rereadTranslation();
  await new Promise((ok) => { const wait = () => (state.item !== it || it.translation ? ok() : setTimeout(wait, 50)); wait(); });
  if (state.item !== it) return;
  const t = it.translation;
  if (t.error) { toast(tx('仍然读不到翻译状态，没有发送翻译请求')); return; }
  if (['not_requested', 'failed', 'unavailable'].includes(t.status) && t.reason !== 'source_is_chinese') requestTranslation();
}
function translationStatusEl(it, cls = '') {
  const [label, tone, action] = translationLine(it);
  return h('div', { class: `status-line ${cls}` }, h('span', { class: `dot ${tone}` }), h('span', { class: 'grow' }, label),
    action ? h('button', { class: 'btn-sm', onclick: action[1] }, action[0]) : null);
}

let scrollQueued = false;
// 竖排: a vertical wheel (desktop preview, trackpads) moves along the columns, right to left.
function bindVert(pane) {
  pane.addEventListener('wheel', (e) => {
    if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
    e.preventDefault(); pane.scrollLeft -= e.deltaY;
  }, { passive: false });
}
// 竖排 restores a remembered column offset. The columns settle a frame or two later (the pane's
// height decides how many there are) and WebKit then resets a vertical-rl scroller to its origin, so
// the offset is re-applied while the text still changes size, until the reader touches the pane.
function holdVert(p, v) {
  p.scrollLeft = v;
  if (!v || typeof ResizeObserver !== 'function') return;
  let user = false;
  for (const ev of ['pointerdown', 'touchstart', 'wheel', 'keydown']) p.addEventListener(ev, () => { user = true; }, { once: true, passive: true });
  const ro = new ResizeObserver(() => { if (!user && p.isConnected) p.scrollLeft = v; });
  ro.observe(p.firstElementChild || p); setTimeout(() => ro.disconnect(), 1500);
}
// Reader header: same presentation as the list (translation first, separator, source title).
function readerTitle(m, trTitle) {
  const t = (m.translated_title || trTitle || '').trim();
  if (!t || t === m.title) return [h('h1', {}, zhShow(m.title))];
  return [h('h1', { class: 'h-tr' }, zhShow(t)), h('div', { class: 'h-orig' }, h('span', { class: 'tt-sep', 'aria-hidden': 'true' }, '｜'), zhShow(m.title))];
}
// 简繁 (原文 → 简体 → 繁体, display only) and 竖排 / 横排, on Chinese text.
function gujiControls(it) {
  if (!zhItem(it)) return [];
  const g = gj(it.id); const names = { orig: tx('原文'), s: tx('简体'), t: tx('繁体') };
  const zhBtn = h('button', { class: `btn-sm gj-zh${g.zh !== 'orig' ? ' on' : ''}`, 'data-zh': g.zh, 'aria-label': tx('简繁：{0}（点击切换）', names[g.zh]), onclick: async () => {
    const next = { orig: 's', s: 't', t: 'orig' }[gj(it.id).zh];
    if (next !== 'orig') {
      zhBtn.disabled = true;
      try { await zhLoad(next); } catch { zhBtn.disabled = false; toast(tx('简繁词典没有载入，仍显示原文')); return; }
    }
    gjSet(it.id, { zh: next }); if (state.item === it) render();
  } }, tx('简繁'), h('small', {}, names[g.zh]));
  const bi = state.lang === 'bi';
  const vBtn = h('button', { class: `btn-sm gj-vert${g.vertical ? ' on' : ''}`, 'aria-pressed': String(g.vertical), 'data-vertical': g.vertical ? '1' : '0',
    onclick: () => { gjSet(it.id, { vertical: !gj(it.id).vertical }); it.vscroll = 0; render(); } }, g.vertical ? tx('横排') : tx('竖排'));
  return [zhBtn, vBtn, bi && g.vertical ? h('span', { class: 'gj-hint' }, tx('对照时按横排显示')) : null];
}
function gujiNote(it) {
  return zhItem(it) && gj(it.id).zh !== 'orig' ? h('div', { class: 'gj-note', role: 'note' }, tx('转换只影响显示，不修改原文')) : null;
}
function renderItem(id) {
  if (!state.item || state.item.id !== id) { loadItem(id); }
  const it = state.item;
  const back = h('button', { class: 'icon-btn', 'aria-label': tx('返回资料库'), onclick: () => go('/') }, icon('back', 22));
  const seg = h('div', { class: 'seg', role: 'tablist' },
    [['orig', tx('原文')], ['bi', tx('对照')], ['trans', tx('译文')]].map(([k, label]) => h('button', {
      class: state.lang === k ? 'on' : '', role: 'tab', 'aria-selected': state.lang === k ? 'true' : 'false',
      onclick: () => { if (k !== state.lang && !editGuard()) return; state.lang = k; it.scroll = 0; render(); const sc = document.getElementById('reader-scroll'); if (sc) sc.scrollTop = 0; },
    }, label)));
  // 长文阅读 v2: ‹ · ≡ 目录 · 原文/对照/译文 · 重点 · 全部 (then 条目详情).
  const lrB = it.meta ? lrTopButtons(it) : [];
  const top = h('header', { class: `reader-top topbar${lrB.length ? ' lr-top' : ''}` }, back, lrB[0] || null, seg, lrB[1] || null,
    h('button', { class: 'icon-btn', 'aria-label': tx('条目详情'), disabled: !it.meta, onclick: () => openSide('right') }, icon('panel', 22)));

  if (!it.meta) {
    const msg = it.error ? (ERR_TEXT[it.error] || tx('载入失败')) : waitNote(tx('载入中…'), 'wait');
    return h('div', { class: 'screen' }, ribbon(), top,
      h('div', { class: 'empty' }, msg, it.error ? h('div', {}, h('button', { class: 'btn-sm mt', onclick: () => loadItem(id) }, tx('重试'))) : null));
  }

  const m = it.meta;
  const t = it.translation;
  const trTitle = t && !t.error ? (t.translatedTitle || t.title) : null;
  const content = h('div');
  const head = h('section', { class: `reader-head${it.headerCollapsed ? ' collapsed' : ''}` },
    h('div', { class: 'reader-head-content', id: 'reader-head-content', hidden: !!it.headerCollapsed },
    ...readerTitle(m, trTitle),
    pubParts(m).length ? h('div', { class: 'meta pub-line' }, (m.published_at ? pubParts(m) : [tx('时长 {0}', fmtDuration(m.duration_seconds))]).join(' · ')) : null,
    h('div', { class: 'meta' }, [showSource(m.source), m.item_type && typeLabel(m.item_type), m.captured_at && showDate(m.captured_at, 10)].filter(Boolean).join(' · ')),
    it.cache === 'checking' ? h('div', { class: 'cache-note' }, tx('缓存版本 · 正在向 Mac 确认…'))
      : it.cache === 'offline' ? h('div', { class: 'cache-note off' }, tx('离线 · 缓存版本'),
        h('button', { class: 'linkish', onclick: () => loadItem(it.id) }, tx('重试'))) : null,
    aiBar(it), emphasisLine(it),
    gujiNote(it),
    h('div', { class: 'reading-type', role: 'group', 'aria-label': tx('阅读字体') },
      // Aa cycles 无衬线 → 宋体 → 寒蝉拙楷 for this item (宋体 / 无衬线 also become the default for others).
      h('button', { class: `btn-sm${readFont(it) === 'kai' ? ' kai' : readFont(it) === 'serif' ? ' serif' : ''}`, 'aria-label': tx('切换阅读字体'), 'aria-pressed': String(readFont(it) !== 'sans'), 'data-font': readFont(it), onclick: () => {
        const f = { sans: 'serif', serif: 'kai', kai: 'sans' }[readFont(it)];
        if (f !== 'kai') { state.readingFont = f; localStorage.setItem('memex.m.readingFont', f); }
        gjSet(it.id, { font: f }); render();
      } }, 'Aa', readFont(it) === 'kai' ? h('small', {}, tx('寒蝉拙楷')) : null),
      h('button', { class: 'btn-sm', 'aria-label': tx('字号：{0}（点击切换）', state.readingSize), onclick: () => {
        const sizes = [16, 18, 20, 22]; state.readingSize = sizes[(sizes.indexOf(state.readingSize) + 1) % sizes.length];
        localStorage.setItem('memex.m.readingSize', String(state.readingSize)); render();
      } }, `A ${state.readingSize}`),
      ...gujiControls(it),
      // 编辑 Markdown of a book: one side of the chapter on screen (off in 对照, with the reason).
      it.book?.doc && editOn() ? h('button', { class: `btn-sm ce-open${editTarget(it) || it.edit ? '' : ' off'}`, 'aria-disabled': editTarget(it) || it.edit ? null : 'true',
        onclick: () => openEditor(it) }, it.edit ? tx('继续编辑') : tx('编辑 Markdown')) : null,
      ...trControls(it),
      // 格式整理: an ordinary item's body, or the chapter of the side shown (off in 对照, with the reason).
      fmtOn() && (it.book?.doc || !it.book) ? h('button', { class: `btn-sm fmt-open${fmtHere(it) ? '' : ' off'}`, 'aria-disabled': fmtHere(it) ? null : 'true',
        onclick: () => askFormat(it) }, tx('格式整理')) : null)),
    h('button', { class: 'reader-head-fold', 'aria-controls': 'reader-head-content', 'aria-expanded': String(!it.headerCollapsed),
      'aria-label': it.headerCollapsed ? tx('展开阅读工具') : tx('收起阅读工具'),
      onclick: () => { it.headerCollapsed = !it.headerCollapsed; render(); } }, icon('down', 14)));
  if (it.book) return h('div', { class: 'screen' }, ribbon(), top, head, bookBar(it), bookBody(it), renderDrawer(), ...bookPicker(it), ...repairSheet(it), ...xlateSheet(it), ...editSheet(it), ...diffSheet(it), ...(state.fmt?.item_id === it.id ? fmtConfirmSheet(state.fmt) : []));
  // CSSOM, not a style attribute: the gateway's CSP (style-src 'self') refuses inline style attributes.
  // A tap on a 手工标记 (no selection) opens its bar: another style, or 删除.
  const articleEl = h('article', { class: `article${fontCls(it)}`, onclick: (e) => {
    const mk = e.target.closest?.('mark.um[data-um]');
    if (mk && !lrSelRange()) lrMarkBar(it, mk.dataset.um, mk);
  } }, content);
  articleEl.style.setProperty('--reading-size', `${state.readingSize}px`);
  const vert = vertNow(it);
  const article = h('div', { class: `scroll${vert ? ' vert' : ''}`, id: 'reader-scroll', 'data-item': it.id }, articleEl);
  fillArticle(content, article);
  article.addEventListener('scroll', () => { if (!scrollQueued) { scrollQueued = true; requestAnimationFrame(() => { scrollQueued = false; updateReaderChrome(); }); } }, { passive: true });
  if (vert) bindVert(article);
  // 竖排 scrolls sideways from the first (right-most) column: its own remembered offset.
  requestAnimationFrame(() => { if (vert) holdVert(article, it.vscroll || 0); else article.scrollTop = it.scroll || 0; updateReaderChrome(); });
  const knobs = !vert && state.insView === 'text' ? lrKnobs(it) : [];
  return h('div', { class: 'screen' }, ribbon(), top, head, h('div', { class: 'reader-body' }, fmtBanner(it), lrLine(it), article, DRAFTS && !vert ? scrubberEl() : null,
    ...knobs, h('div', { class: 'lr-bubble', id: 'lr-bubble', hidden: true, 'aria-live': 'polite' })), renderDrawer(),
    lrTocEl(it), lrHlMenu(it),
    ...psSheet(it), ...diffSheet(it), ...(state.fmt?.item_id === it.id ? fmtConfirmSheet(state.fmt) : []));
}

// Fills the article body in place; later segments are appended without rebuilding the screen,
// so the reading position and the chat draft survive.
function fillArticle(content, scroller) {
  const it = state.item;
  const t = it.translation;
  const ready = t && !t.error && t.status === 'succeeded';
  let paras;
  const original = () => [...completeBlocks(it.text, it.nextByte < it.total).map((b, i) => blockEl(b, '', i)),
    it.nextByte < it.total ? h('div', { class: 'note', id: 'more-text' }, tx('继续载入正文…')) : null];
  const noTranslation = state.lang !== 'orig' && !ready;
  if (state.insView !== 'text') {
    // 金句 / 事实核查 replace the body (the title block stays), as on the desktop.
    content.replaceChildren(state.insView === 'quotes' ? quotesPanel(it) : factsPanel(it));
    return;
  }
  if (state.lang === 'orig') {
    paras = original();
  } else if (noTranslation) {
    // Never a blank page: say why there is no translation, then the whole original (still
    // loading by segment). 翻译此文 only on an explicit tap; never requested automatically.
    paras = [fallbackNote(it), ...original()];
  } else if (state.lang === 'trans') {
    paras = [...completeBlocks(t.text, t.next != null).map((b, i) => blockEl(b, '', i, 't')), t.next != null ? h('div', { class: 'note', id: 'more-text' }, tx('继续载入译文…')) : null];
  } else if (it.nextByte < it.total || t.next != null) {
    if (!it.fullFailed) loadFull();
    paras = [h('div', { class: 'note' }, it.fullFailed ? tx('全文没有载入完整，暂时无法逐段对照。') : tx('正在载入全文，以便逐段对照…'))];
  } else {
    const rows = pairBlocks(splitBlocks(it.text), splitBlocks(t.text));
    paras = rows
      ? rows.map((r, i) => (r.shared ? blockEl(r.shared, '', i) : h('div', { class: `bi-pair${isNoteText(r.o.text) && isNoteText(r.t.text) ? ' lnote' : ''}` }, blockEl(r.o, '', i), blockEl(r.t, 'tr', i, 't'))))
      : [h('div', { class: 'note' }, tx('译文的段落与原文对不上，为避免错位，这里只显示译文。')), ...splitBlocks(t.text).map((b, i) => blockEl(b, '', i, 't'))];
  }
  // 正文结尾 and the saved AI conversation (read only) close a fully loaded article (horizontal).
  const tail = !vertNow(it) && it.nextByte >= it.total && (state.lang === 'orig' || noTranslation || state.lang === 'trans' || t?.next == null) ? lrEndEls(it) : [];
  // AI 纪要 is the first row of the article, above the body (desktop: the list's lead row).
  // 条目封面 sits above everything, outside the body's blocks (竖排: not shown).
  content.replaceChildren(...[coverEl(it), vertNow(it) ? null : minutesCard(it), ...paras, ...tail].filter(Boolean)); // 竖排: 纪要 stays in 条目详情
  markNoteRuns(content);
  bindImages(content, scroller, it);
  chromeKey = null; requestAnimationFrame(updateReaderChrome);
  const more = content.querySelector('#more-text');
  if (!more) return;
  const io = new IntersectionObserver(async (es) => {
    if (!es.some((e) => e.isIntersecting)) return;
    io.disconnect();
    const id = it.id;
    try { await (state.lang === 'orig' || noTranslation ? loadSegment() : loadTranslationPage()); noteOutcome(null); } catch (e) { noteOutcome(e); toast(ERR_TEXT[e.code] || tx('载入失败')); }
    if (state.item?.id === id && content.isConnected) fillArticle(content, scroller);
  }, { root: scroller, rootMargin: '400px' });
  io.observe(more);
}

// ---------- sidebars ----------
// Left: library navigation (all items, types, sources). Right: the open item's details, its
// translation and AI 纪要. Both overlay the screen (and the chat drawer) with a scrim; tap the
// scrim, the close button or Escape to close.
function openSide(which) { state.side = which; state.sideShown = null; render(); }
function closeSide() { state.side = null; render(); }
addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && state.item?.diff) { state.item.diff = null; render(); }
  else if (e.key === 'Escape' && state.item?.edit?.open) { state.item.edit.open = false; render(); }
  else if (e.key === 'Escape' && state.side) closeSide();
  else if (e.key === 'Escape' && state.item?.book?.picker) { state.item.book.picker = false; render(); }
  else if (e.key === 'Escape' && state.item?.repairAsk) { state.item.repairAsk = null; render(); }
  else if (e.key === 'Escape' && state.item?.xlateAsk) { state.item.xlateAsk = null; render(); }
});
function sidePanel(which, label, ...body) {
  const still = state.sideShown === which; // re-render without replaying the slide-in
  state.sideShown = which;
  return [
    h('div', { class: `side-scrim${still ? ' still' : ''}`, onclick: closeSide }),
    h('aside', { class: `side ${which}${still ? ' still' : ''}`, role: 'dialog', 'aria-modal': 'true', 'aria-label': label },
      h('div', { class: 'side-head' }, h('div', { class: 'side-title' }, label),
        h('button', { class: 'icon-btn', 'aria-label': tx('关闭'), onclick: closeSide }, icon('x', 20))),
      h('div', { class: 'side-body', id: `side-${which}` }, ...body)),
  ];
}
// Draft left sidebar: foldable 分组 / 类型 / 来源 (fold state kept on this phone), the Mac's group
// tree with counts, sources grouped by site, types in words. Mirrors the desktop LibrarySidebar.
const FOLD_KEY = 'memex.m.fold';
const folds = () => { try { return JSON.parse(localStorage.getItem(FOLD_KEY)) || {}; } catch { return {}; } };
const isFolded = (k, def = false) => folds()[k] ?? def;
function toggleFold(k, def = false) { const f = folds(); f[k] = !(f[k] ?? def); localStorage.setItem(FOLD_KEY, JSON.stringify(f)); render(); }
// memex.library.groups.tree, all pages (≤ 10 × 500 nodes), restarted once on a stale view.
// The last tree read from the Mac is kept in the pairing-bound cache: a relaunched app paints it at
// once (labelled 缓存版本) while the Mac is asked again, and keeps it when the Mac can't answer.
// Groups already on screen stay on screen during a re-read (after a move or a trash edit).
let groupsLoading = null; let groupsAsked = false;
async function primeGroups() {
  const c = await cacheKv('groups');
  if (!state.groups && c?.nodes?.length) state.groups = { state: c.state, nodes: c.nodes, cached: true };
}
function loadGroups() {
  groupsAsked = true;
  return (groupsLoading ??= readGroups().finally(() => { groupsLoading = null; }));
}
async function readGroups() {
  if (!state.groups?.nodes.length) await primeGroups();
  const shown = state.groups?.nodes.length ? { state: state.groups.state, nodes: state.groups.nodes, cached: state.groups.cached } : null;
  state.groups = shown || { state: 'loading', nodes: [] }; render();
  try {
    await caps();
    if (GATEWAY && state.caps?.groups !== 'available') { state.groups = { state: 'off', nodes: [] }; render(); return; }
    let nodes = []; let view = null; let offset = 0; let restarted = false;
    for (let page = 0; page < 10 && offset != null; page += 1) {
      let r;
      try { r = await source.groupsTree({ offset, limit: 500, expected_view: view }); } catch (e) {
        if (e.code === 'stale_group_view' && !restarted) { restarted = true; nodes = []; view = null; offset = 0; continue; }
        throw e;
      }
      nodes.push(...r.nodes); view = r.view; offset = r.next_offset;
    }
    state.groups = { state: offset == null ? 'ok' : 'partial', nodes };
    cacheKv('groups', { state: state.groups.state, nodes });
    noteOutcome(null);
  } catch (e) {
    noteOutcome(e);
    state.groups = shown ? { ...shown, err: e.code || 'unknown' } : { state: 'err', nodes: [], err: e.code || 'unknown' };
  }
  if (state.side === 'left') render();
}
let groupMove = null;
async function moveGroup(node, before_id, retry = false) {
  if (state.caps?.group_reorder !== 'available' || groupMove?.busy || (groupMove && !retry)) return;
  groupMove ||= { params: { group_id: node.id, before_id, expected_version: node.version, request_id: crypto.randomUUID() } };
  groupMove.busy = true; render();
  try {
    await source.reorderGroup(groupMove.params);
    groupMove = null;
  } catch (e) {
    if (e.code === 'unknown' || e.code === MAC_BUSY) groupMove.busy = false;
    else { groupMove = null; toast(tx('排序未保存，请刷新后重试')); }
  }
  await loadGroups();
}
function groupDragHandle(node) {
  const handle = h('span', { class: 'group-drag', role: 'button', tabindex: '0', 'aria-label': tx('拖动排序：{0}', node.name),
    onclick: (e) => e.stopPropagation(),
    onkeydown: (e) => {
      if (!['ArrowUp', 'ArrowDown'].includes(e.key)) return;
      e.preventDefault(); e.stopPropagation();
      const roots = state.groups.nodes.filter((n) => !n.parent_id);
      const at = roots.findIndex((n) => n.id === node.id);
      const target = e.key === 'ArrowUp' ? roots[at - 1] : roots[at + 2];
      if (e.key === 'ArrowUp' && (!target || (target.kind === 'user' && target.name.toLowerCase() === 'main'))) return;
      moveGroup(node, target?.id ?? null);
    } }, '⠿');
  let active = false; let moved = false; let startY = 0; let before = null;
  const clear = () => document.querySelectorAll('.group-drop').forEach((el) => el.classList.remove('group-drop'));
  handle.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || groupMove) return;
    e.preventDefault(); e.stopPropagation(); active = true; startY = ptY(e); moved = false;
    handle.setPointerCapture(e.pointerId);
  });
  handle.addEventListener('pointermove', (e) => {
    if (!active) return;
    moved ||= Math.abs(ptY(e) - startY) > 6;
    if (!moved) return;
    clear();
    const rows = [...document.querySelectorAll('[data-root-sort]')].filter((r) => r.dataset.rootSort !== node.id);
    const target = rows.find((r) => ptY(e) < rectIn(r).top + rectIn(r).height / 2);
    before = target?.dataset.rootSort ?? null;
    (target || document.querySelector('.group-order-end'))?.classList.add('group-drop');
    const scroller = document.querySelector('.library-nav-main');
    const box = rectIn(scroller);
    if (ptY(e) < box.top + 35) scroller.scrollTop -= 12;
    if (ptY(e) > box.bottom - 35) scroller.scrollTop += 12;
  });
  handle.addEventListener('pointerup', (e) => {
    if (!active) return;
    e.stopPropagation(); active = false; clear();
    if (moved) moveGroup(node, before);
  });
  handle.addEventListener('pointercancel', () => { active = false; clear(); });
  return handle;
}
function renderLeftSide() {
  if (!DRAFTS) return renderLeftSideV1();
  const L = state.lib; const q = L.query;
  const all = !q.source && !q.item_type && !q.group_id && route().name !== 'trash';
  if (!state.groups || !groupsAsked) setTimeout(loadGroups);
  const row = (on, label, n, fn, depth = 0, lead = null) => h('button', { class: `nav-row${on ? ' on' : ''}`, onclick: fn, 'aria-current': on ? 'true' : null, style: null, 'data-depth': String(depth) },
    lead, h('span', { class: 'grow lbl' }, label), n == null ? null : h('span', { class: 'n' }, n));
  const sec = (key, label, count, body, def = false) => {
    const folded = isFolded(key, def);
    return h('section', { class: 'nav-sec' },
      h('button', { class: 'nav-sec-head', 'aria-expanded': String(!folded), onclick: () => toggleFold(key, def) },
        icon(folded ? 'chev' : 'down', 14), h('span', { class: 'grow' }, label), folded && count != null ? h('span', { class: 'n' }, count) : null),
      folded ? null : body);
  };
  const twisty = (key, def) => h('span', { class: 'twisty', role: 'button', 'aria-label': isFolded(key, def) ? tx('展开') : tx('收起'),
    onclick: (e) => { e.stopPropagation(); toggleFold(key, def); } }, icon(isFolded(key, def) ? 'chev' : 'down', 13));
  // 分组: roots then folders, each foldable; a node shows its subtree count and selects its subtree.
  const G = state.groups || { state: 'loading', nodes: [] };
  const kids = new Map();
  for (const n of G.nodes) { const k = n.parent_id ?? ''; if (!kids.has(k)) kids.set(k, []); kids.get(k).push(n); }
  const node = (n, depth) => {
    const ch = kids.get(n.id) || []; const key = `grp:${n.id}`;
    const hasTrash = !n.parent_id && trashServed();
    const r = row(q.group_id === n.id && route().name !== 'trash', n.name, n.subtree_item_count, () => pickFacet(null, null, n), depth,
      ch.length || hasTrash ? twisty(key, true) : h('span', { class: 'twisty none' }));
    const pinned = !n.parent_id && n.kind === 'user' && n.name.toLowerCase() === 'main';
    if (pinned) r.append(h('span', { 'aria-label': tx('固定第一位') }, '⌖'));
    else if (!n.parent_id && state.caps?.group_reorder === 'available') {
      r.dataset.rootSort = n.id;
      r.append(groupDragHandle(n));
    }
    return [r,
    ...(!isFolded(key, true) ? [...ch.flatMap((c) => node(c, depth + 1)),
      hasTrash ? row(route().name === 'trash' && state.trash?.root.id === n.id, tx('垃圾桶'), null, () => openTrash(n), depth + 1, icon('trash', 15)) : null] : [])];
  };
  const groupsBody = G.state === 'loading' ? [h('div', { class: 'nav-hint' }, h('span', { class: 'spin' }), tx('正在读取分组…'))]
    : G.state === 'off' ? [h('div', { class: 'nav-hint' }, tx('这台 Mac 的网关还不提供分组'))]
      : G.state === 'err' ? [h('div', { class: 'nav-hint warn' }, tx('分组读取失败：{0}', reasonText(G.err)), ' ', h('button', { class: 'linkish', onclick: loadGroups }, tx('重试')))]
        : !G.nodes.length ? [h('div', { class: 'nav-hint' }, tx('还没有分组'))]
          : [...(kids.get('') || []).flatMap((n) => node(n, 0)),
            h('div', { class: 'group-order-end' }),
            groupMove ? h('div', { class: 'nav-hint' }, groupMove.busy ? tx('正在保存排序…') : tx('排序结果未知'),
              groupMove.busy ? null : h('button', { class: 'linkish', onclick: () => moveGroup(null, null, true) }, tx('确认同一次排序'))) : null,
            G.state === 'partial' ? h('div', { class: 'nav-hint' }, tx('分组太多，只显示了前一部分')) : null,
            G.err ? h('div', { class: 'nav-hint warn' }, G.cached ? tx('离线 · 缓存版本') : tx('分组读取失败：{0}', reasonText(G.err)), ' ', h('button', { class: 'linkish', onclick: loadGroups }, tx('重试')))
              : G.cached ? h('div', { class: 'nav-hint' }, h('span', { class: 'spin' }), tx('缓存版本 · 正在向 Mac 确认…')) : null];
  const S = groupSources(L.sources);
  const srcRow = (f, depth) => row(q.source === f.value, f.sub, f.count, () => pickFacet(f.value, null), depth, h('span', { class: 'twisty none' }));
  const sourcesBody = [
    ...S.groups.flatMap((g) => {
      const key = `src:${g.key}`;
      const one = g.members.length === 1;
      const on = g.members.some((m) => m.value === q.source);
      return [row(on && one, g.name, g.count, one ? () => pickFacet(g.members[0].value, null) : () => toggleFold(key, true), 0,
        one ? h('span', { class: 'twisty none' }) : twisty(key, true)),
      ...(!one && !isFolded(key, true) ? g.members.map((m) => srcRow(m, 1)) : [])];
    }),
    S.others.length ? row(false, tx('其他来源'), S.othersCount, () => toggleFold('src:__others', true), 0, twisty('src:__others', true)) : null,
    ...(S.others.length && !isFolded('src:__others', true) ? S.others.map((m) => srcRow({ ...m, sub: sourceLabel(m.value) }, 1)) : []),
  ];
  const typesBody = L.item_types.map((f) => row(q.item_type === f.value, typeLabel(f.value), f.count, () => pickFacet(null, f.value), 0, h('span', { class: 'twisty none' })));
  return sidePanel('left', tx('资料库'),
    h('div', { class: 'library-nav-main' },
      row(all, tx('全部条目'), all ? L.total : null, () => pickFacet(null, null), 0, h('span', { class: 'twisty none' }, icon('lib', 15))),
      sec('groups', tx('分组'), G.nodes.filter((n) => !n.parent_id).length || null, groupsBody),
      L.item_types.length ? sec('types', tx('类型'), L.item_types.length, typesBody) : null,
      all ? null : h('div', { class: 'small side-note' }, tx('类型和来源的数量按当前筛选统计。'))),
    L.sources.length ? h('div', { class: 'library-nav-sources' },
      sec('sources', tx('来源'), S.groups.length + (S.others.length ? 1 : 0), sourcesBody)) : null);
}
function renderLeftSideV1() {
  const L = state.lib; const q = L.query;
  const all = !q.source && !q.item_type;
  const navBtn = (on, label, n, fn) => h('button', { class: `nav-row${on ? ' on' : ''}`, onclick: fn, 'aria-current': on ? 'true' : null },
    h('span', { class: 'grow' }, label), n == null ? null : h('span', { class: 'n' }, n));
  return sidePanel('left', tx('资料库'),
    navBtn(all, tx('全部条目'), all ? L.total : null, () => pickFacet(null, null)),
    L.item_types.length ? h('div', { class: 'nav-group' }, tx('类型')) : null,
    L.item_types.map((f) => navBtn(q.item_type === f.value, f.value, f.count, () => pickFacet(null, f.value))),
    L.sources.length ? h('div', { class: 'nav-group' }, tx('来源')) : null,
    L.sources.map((f) => navBtn(q.source === f.value, showSource(f.value), f.count, () => pickFacet(f.value, null))),
    all ? null : h('div', { class: 'small side-note' }, tx('类型和来源的数量按当前筛选统计。')));
}
// ---------- AI 纪要 card (reader, above the body) ----------
// Mirrors the desktop minutes card: manual minutes only. 未生成 → 生成纪要; queued/running → state
// line; succeeded → text, collapsible (收起/展开) and capped until 展开全部; failed → reason + 重试.
// The backend's summary job *is* the minutes (summary prompt v2), so this card is the 摘要.
function minutesCard(it) {
  if (it.minutesHidden) return null;
  const mm = it.minutes; const ask = it.mAsk;
  let line = null; let tone = 'off'; let action = null; let text = null;
  if (ask === 'requesting') { line = tx('正在请求纪要…'); tone = 'run'; } else if (!mm) line = tx('读取中…');
  else if (mm.error) line = ERR_TEXT[mm.error] || tx('纪要状态暂不可用');
  else if (mm.state === 'succeeded' && mm.text) text = mm.text;
  else if (mm.state === 'queued') { line = tx('排队中…'); tone = 'run'; } else if (mm.state === 'running') { line = tx('正在生成纪要…'); tone = 'run'; } else if (mm.state === 'failed') { line = [tx('生成失败'), reasonText(mm.reason)].filter(Boolean).join(' · '); tone = 'bad'; action = [tx('重试'), requestMinutes]; } else if (mm.state === 'not_requested' && mm.reason === 'minutes_not_applicable') line = reasonText(mm.reason);
  else if (mm.state === 'not_requested') { line = tx('还没有纪要'); action = [tx('生成纪要'), requestMinutes]; } else line = reasonText(mm.reason) || tx('纪要暂不可用');
  if (ask?.error) {
    line = ask.error === 'unknown' ? tx('请求结果未知，已向 Mac 读回状态（不会自动重发）')
      : ask.error === 'invalid_request' ? tx('这台 Mac 的网关还不支持纪要请求') : tx('请求失败：{0}', reasonText(ask.error));
    tone = 'bad';
  }
  const collapsed = !!it.minutesCollapsed && !!text;
  const full = !!it.minutesFull;
  const toggle = () => { it.minutesCollapsed = !collapsed; render(); };
  return h('section', { class: `minutes-card${tone === 'bad' ? ' bad' : ''}`, 'aria-label': tx('AI 纪要') },
    h(text ? 'button' : 'div', { class: 'mc-head', onclick: text ? toggle : null, 'aria-expanded': text ? String(!collapsed) : null },
      h('span', { class: 'ai-badge' }, 'AI'), h('span', { class: 'mc-title' }, tx('AI 纪要')),
      h('span', { class: 'mc-sub' }, tx('摘要')),
      text ? h('span', { class: 'mc-fold' }, collapsed ? tx('展开') : tx('收起'), icon(collapsed ? 'chev' : 'down', 14)) : null),
    line ? h('div', { class: 'status-line' }, h('span', { class: `dot ${tone}` }), h('span', { class: 'grow' }, line),
      action ? h('button', { class: 'btn-sm', onclick: action[1] }, action[0]) : null) : null,
    text && !collapsed ? h('div', { class: `minutes${full ? ' open' : ''}` }, mdDoc(text, { breaks: true })) : null,
    text && !collapsed ? h('button', { class: 'more-btn', onclick: () => { it.minutesFull = !full; render(); } }, full ? tx('收回') : tx('展开全部')) : null);
}

// ---------- 重点标注 / 金句 / 事实核查 (desktop insights.rs semantics) ----------
// Button states: 未生成 / 生成中 / 已完成 / 失败. 重点标注 done → toggles emphasis on the original;
// 金句 / 事实核查 → switch the body to their view (pressing again returns to the text); a kind that
// is not generated or failed is requested on press. Nothing is shown as done unless `succeeded`.
const INSIGHT_KINDS = [['highlights', '重点标注'], ['quotes', '金句'], ['fact_check', '事实核查']];
// Done with items. A done kind whose items the Mac could not fit in one reply has none here.
const insightDone = (kind) => { const s = state.item?.insights?.[kind]; return s?.state === 'succeeded' && Array.isArray(s.items) ? s.items : null; };
function insightState(it, kind) {
  if (it.insAsk?.has(kind)) return ['run', tx('生成中')];
  if (it.insErr?.[kind]) return ['bad', tx('失败'), it.insErr[kind]];
  const s = it.insights?.[kind];
  if (!s && it.insightsError) return ['read', tx('读取失败'), tx('读取 AI 结果失败：{0}', reasonText(it.insightsError))];
  if (!s) return ['off', tx('未生成')];
  if (s.state === 'succeeded' && !Array.isArray(s.items)) return ['big', tx('无法显示'), tx('内容过多，手机上暂时无法显示，请在 Mac 上查看')];
  if (BUSY.has(s.state)) return ['run', tx('生成中')];
  if (s.state === 'succeeded') return ['ok', tx('已完成')];
  if (s.state === 'failed') return ['bad', tx('失败'), reasonText(s.reason) || tx('代理未说明原因')];
  if (s.state === 'unavailable' && s.reason === 'insight_translation_changed') return ['stale', tx('已过期'), reasonText(s.reason)];
  if (s.state === 'unavailable') return ['bad', tx('不可用'), reasonText(s.reason) || tx('代理未说明原因')];
  return ['off', tx('未生成')];
}
async function loadInsights() {
  const it = state.item;
  if (!it) return;
  try { await caps(); } catch { /* offline: try the read itself, it reports honestly */ }
  if (state.item !== it) return;
  if (!insightsOn()) { softRender(); return; } // this Mac's gateway serves no insights: chips stay hidden
  try { it.insights = await source.insightsStatus(it.id); it.insightsError = null; noteOutcome(null); } catch (e) { noteOutcome(e); it.insightsError = e.code || 'unknown'; }
  if (state.item === it && route().name === 'item') softRender();
  schedulePoll();
}
async function requestInsight(kind) {
  const it = state.item;
  it.insAsk ??= new Set(); it.insErr ??= {};
  if (it.insAsk.has(kind)) return;
  delete it.insErr[kind]; it.insAsk.add(kind); render();
  try {
    // The request reply carries states only (gateway): the items are always read back.
    await source.insightsRequest(it.id, [kind]); noteOutcome(null);
    it.insAsk.delete(kind);
    await loadInsights();
  } catch (e) {
    noteOutcome(e);
    it.insErr[kind] = e.code === 'unknown' ? tx('请求结果未知，已向 Mac 读回状态（不会自动重发）') : tx('请求失败：{0}', reasonText(e.code));
    it.insAsk.delete(kind);
    if (e.code === 'unknown') await loadInsights();
  }
  it.insAsk.delete(kind);
  if (state.item === it) render();
  schedulePoll();
}
function pressInsight(it, kind) {
  const [st] = insightState(it, kind);
  if (kind === 'highlights') {
    if (st === 'ok') { state.emphasis = !state.emphasis; state.insView = 'text'; render(); return; }
    state.emphasis = true; state.insView = 'text';
  } else {
    const view = kind === 'quotes' ? 'quotes' : 'facts';
    if (state.insView === view) { state.insView = 'text'; render(); return; }
    state.insView = view;
  }
  if (st === 'read') { loadInsights(); render(); return; }
  // Stale (translation changed) is regenerated only on this explicit press, like 未生成.
  if (st === 'off' || st === 'bad' || st === 'stale') requestInsight(kind); else render();
}
function pressMinutes(it) {
  const mm = it.minutes;
  if (state.insView !== 'text' && mm?.state === 'succeeded') { state.insView = 'text'; it.minutesHidden = false; render(); return; }
  if (mm?.state === 'succeeded') { it.minutesHidden = !it.minutesHidden; render(); return; }
  if (it.mAsk === 'requesting' || BUSY.has(mm?.state)) return;
  if (mm?.state === 'failed' || mm?.state === 'not_requested') requestMinutes();
}
function aiBar(it) {
  if (!insightsOn()) return null;
  const mm = it.minutes;
  const mState = it.mAsk === 'requesting' || BUSY.has(mm?.state) ? ['run', tx('生成中')] : mm?.state === 'succeeded' ? ['ok', tx('已完成')]
    : mm?.state === 'failed' || it.mAsk?.error ? ['bad', tx('失败')] : ['off', tx('未生成')];
  const chip = (label, [st, stLabel, why], on, onclick) => h('button', { class: `ai-chip ${st}${on ? ' on' : ''}`, onclick,
    title: `${label} · ${stLabel}${why ? ` · ${why}` : ''}`, 'aria-label': `${label} · ${stLabel}`, 'aria-pressed': String(!!on) },
    label, h('span', { class: `ai-status ${st}`, 'aria-hidden': 'true' }));
  return h('div', { class: 'ai-bar', role: 'toolbar', 'aria-label': tx('AI 工具') },
    chip(tx('纪要'), mState, mState[0] === 'ok' && !it.minutesHidden && state.insView === 'text', () => pressMinutes(it)),
    INSIGHT_KINDS.map(([k, label]) => chip(tx(label), insightState(it, k),
      k === 'highlights' ? state.emphasis && !!insightDone(k) && state.insView === 'text' : state.insView === (k === 'quotes' ? 'quotes' : 'facts'),
      () => pressInsight(it, k))),
    GATEWAY ? null : h('span', { class: 'tag-demo' }, tx('合成示例')));
}
// The line under the toolbar while 重点标注 is on: the legend, or why it is not shown.
function emphasisLine(it) {
  if (!state.emphasis || state.insView !== 'text' || !insightsOn()) return null;
  if (it.book) return h('div', { class: 'em-line' }, icon('info', 13), tx('按章阅读时暂不显示重点标注'));
  const [st, , why] = insightState(it, 'highlights');
  if (st === 'bad') return h('div', { class: 'em-line warn' }, icon('warn', 13), tx('重点标注生成失败：{0}', why));
  if (st === 'stale') return h('div', { class: 'em-line warn' }, icon('warn', 13), why, ' ', h('button', { class: 'linkish', onclick: () => requestInsight('highlights') }, tx('重新生成')));
  if (st === 'read') return h('div', { class: 'em-line warn' }, icon('warn', 13), why);
  if (st === 'big') return h('div', { class: 'em-line' }, icon('info', 13), why);
  if (st === 'run') return h('div', { class: 'em-line' }, h('span', { class: 'spin' }), tx('重点标注生成中…'));
  if (!insightDone('highlights')) return null;
  const sides = it.insights?.highlights?.sides;
  const transless = sides === 'original_only' || (sides == null && !insightDone('highlights').some((x) => x.translation));
  if (state.lang !== 'orig' && transless) return h('div', { class: 'em-line' }, icon('info', 13), state.lang === 'trans'
    ? tx('译文这一侧还没有重点标注，只有原文有；请切换到原文或对照') : tx('译文这一侧还没有重点标注，只标在原文上'));
  return h('div', { class: 'em-line' },
    h('mark', { class: 'hl bold' }, tx('加粗')), '=', tx('要点'), ' · ', h('mark', { class: 'hl highlight' }, tx('黄底')), '=', tx('关键信息'), ' · ',
    h('mark', { class: 'hl positive' }, tx('蓝')), '=', tx('正面'), ' · ', h('mark', { class: 'hl negative' }, tx('红')), '=', tx('负面'),
    h('span', { class: 'em-note' }, tx('AI 生成，仅供参考')));
}
// 金句 / 事实核查 while not done: generating, failed (with 重试), or not generated (生成).
function insightWaiting(it, kind, label) {
  const [st, , why] = insightState(it, kind);
  if (st === 'ok') return null;
  const [text, action] = st === 'run' ? [tx('{0}生成中…', label), null] : st === 'bad' ? [tx('{0}生成失败：{1}', label, why), tx('重试')]
    : st === 'read' ? [why, tx('重新读取')] : st === 'big' ? [why, null] : [tx('{0}尚未生成', label), tx('生成')];
  return h('div', { class: 'ins-wait' }, h('div', { class: 'ins-wait-line' }, st === 'run' ? h('span', { class: 'spin' }) : icon(st === 'bad' || st === 'read' ? 'warn' : 'info', 16), text),
    action ? h('button', { class: 'btn-sm', onclick: () => (st === 'read' ? loadInsights() : requestInsight(kind)) }, action) : null);
}
function locate(idx) {
  state.insView = 'text'; render();
  requestAnimationFrame(() => {
    const el = document.querySelector(`#reader-scroll [data-b="${idx}"]`);
    if (!el) { toast(tx('这一段还没有载入')); return; }
    el.scrollIntoView({ block: 'center', behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
    el.classList.add('located'); setTimeout(() => el.classList.remove('located'), 1600);
  });
}
const backToText = () => h('button', { class: 'linkish back-text', onclick: () => { state.insView = 'text'; render(); } }, icon('back', 14), tx('返回正文'));
// 金句: one card per quote, the verbatim words large, the Chinese beneath, why + 定位原文.
function quotesPanel(it) {
  const label = tx('金句');
  const head = h('div', { class: 'ins-head' }, h('span', { class: 'ai-badge' }, 'AI'), label, backToText());
  const wait = insightWaiting(it, 'quotes', label);
  if (wait) return h('div', { class: 'ins-panel' }, head, wait);
  const items = insightDone('quotes');
  return h('div', { class: 'ins-panel' }, head,
    items.length ? items.map((q) => h('figure', { class: 'quote-card' },
      h('blockquote', { class: 'q-orig' }, `“${q.quote}”`),
      q.translation_zh?.trim() ? h('div', { class: 'q-zh' }, q.translation_zh) : null,
      h('figcaption', { class: 'q-foot' }, h('span', { class: 'grow' }, q.why || ''),
        h('button', { class: 'locate', onclick: () => locate(q.block_index) }, icon('search', 13), tx('定位原文')))))
      : h('div', { class: 'ins-empty' }, tx('没有找到金句')));
}
// 事实核查: each claim with its verdict, confidence and explanation (model knowledge, no web).
const VERDICT = { supported: tx('支持'), disputed: tx('存疑'), incorrect: tx('有误'), unverifiable: tx('无法核实'), opinion: tx('观点') };
const CONFIDENCE = { high: tx('置信度高'), medium: tx('置信度中'), low: tx('置信度低') };
function factsPanel(it) {
  const label = tx('事实核查');
  const head = h('div', { class: 'ins-head' }, h('span', { class: 'ai-badge' }, 'AI'), label, backToText());
  const wait = insightWaiting(it, 'fact_check', label);
  if (wait) return h('div', { class: 'ins-panel' }, head, wait);
  const items = insightDone('fact_check');
  return h('div', { class: 'ins-panel' }, head,
    it.insights?.fact_check?.basis === 'model_knowledge' ? h('div', { class: 'basis' }, icon('info', 13), tx('基于模型知识，未联网核查'), h('span', { class: 'em-note' }, tx('AI 生成，仅供参考'))) : null,
    items.length ? items.map((f) => h('article', { class: `fact-card v-${VERDICT[f.verdict] ? f.verdict : 'other'}` },
      h('div', { class: 'f-top' }, h('span', { class: 'verdict' }, VERDICT[f.verdict] || f.verdict),
        h('span', { class: 'conf' }, CONFIDENCE[f.confidence] || f.confidence || ''),
        h('button', { class: 'locate', onclick: () => locate(f.block_index) }, icon('search', 13), tx('定位原文'))),
      h('div', { class: 'f-quote' }, `“${f.quote}”`),
      h('div', { class: 'f-why' }, f.explanation)))
      : h('div', { class: 'ins-empty' }, tx('没有可核查的陈述')));
}

function renderRightSide() {
  const it = state.item;
  const m = it?.meta;
  if (!m) return [];
  const t = it.translation;
  const trTitle = t && !t.error ? (t.translatedTitle || t.title) : null;
  const kv = (k, v) => h('div', { class: 'kv' }, h('span', { class: 'k' }, k), h('span', { class: 'v' }, v));
  return sidePanel('right', tx('条目详情'),
    h('section', { class: 'side-sec' },
      h('div', { class: 'item-title' }, m.title),
      trTitle && trTitle !== m.title ? h('div', { class: 'tr-title' }, trTitle) : null,
      kv(tx('类型'), m.item_type ? typeLabel(m.item_type) : tx('未知')),
      kv(tx('来源'), showSource(m.source)),
      kv(tx('添加日期'), showDate(m.captured_at, 16)),
      h('div', { class: 'tags' }, m.manual_tags.length ? m.manual_tags.map((tag) => h('span', { class: 'tagc' }, tag)) : h('span', { class: 'small' }, tx('没有标签'))),
      h('button', { class: 'btn edit-btn', onclick: () => { state.side = null; openSheet(); } }, tx('编辑标题和标签'))),
    it.book?.doc ? h('section', { class: 'side-sec' }, h('div', { class: 'sec-head' }, tx('版本')),
      h('div', { class: 'status-line' }, h('span', { class: 'dot ok' }), h('span', { class: 'grow' },
        tx('原文（{0}）与译文（{1}）按章节配对', langName(it.book.o.language), langName(it.book.t.language)))))
      : h('section', { class: 'side-sec' }, h('div', { class: 'sec-head' }, tx('翻译')), translationStatusEl(it)));
}

// Item chat. The Mac stores every successful turn (from this phone or the Mac app) per item; the
// drawer shows that stored conversation and continues it, sending it as context. Local turns not
// yet read back are kept after it, matched by request id. A failed or timed-out answer keeps the
// question; 重试 re-sends it with the same request id, so the Mac returns the stored answer if the
// first attempt did complete (never a second exchange). Each reply lands on the item and the
// question it was asked for; switching items never moves it.
const CHAT_ERR = {
  no_chat_provider: tx('Mac 上还没有设置对话模型'),
  provider_failure: tx('对话模型返回错误，请稍后重试'),
  provider_outcome_unknown: tx('没有收到回答（可能已经调用了模型），可以重试'),
  unavailable: tx('无法连接到你的 Mac'),
  daemon_unavailable: tx('Mac 暂时没有响应，请稍后重试'),
  library_resource_not_found: tx('条目不存在或无权访问'),
};
const chatTime = () => new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
function chatFor(id) {
  state.chats ??= {};
  // hist: null (not read) | 'loading' | 'cached' | 'ok' | 'err' | 'off' (not served)
  return (state.chats[id] ??= { msgs: [], stored: [], local: [], hist: null, histErr: null, partial: false, pending: false, error: null });
}
const turnTime = (ms) => {
  const d = new Date(ms);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : d.toLocaleString([], { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' });
};
// Stored turns first (Mac order), then local turns whose request id the Mac has not returned.
function mergeHistory(c) {
  const known = new Set(c.stored.map((t) => t.request_id));
  c.local = c.local.filter((m) => !known.has(m.rid));
  c.msgs = [...c.stored.map((t) => ({ role: t.role, content: t.content, time: turnTime(t.created_at_ms), rid: t.request_id, model: t.model, stored: true, cut: !!t.truncated,
    restored: t.origin === 'restored_from_user_screenshot' })), ...c.local];
}
async function loadHistory(id) {
  const c = chatFor(id);
  if (c.hist === 'loading') return;
  try { await caps(); } catch { /* offline: the read below reports it */ }
  if (!historyOn()) { c.hist = 'off'; return; }
  c.hist = c.hist === 'cached' ? 'cached' : 'loading'; c.histErr = null;
  if (state.item?.id === id) softRender();
  try {
    const r = await source.conversationRead(id);
    c.stored = r.turns; c.partial = !!r.partial; c.hist = 'ok';
    mergeHistory(c);
    if (state.item?.id === id) cacheSoon();
  } catch (e) {
    c.hist = c.hist === 'cached' ? 'cached' : 'err'; c.histErr = e.code || 'unknown';
  }
  if (state.item?.id === id) { softRender(); document.getElementById('chat-scroll')?.scrollTo({ top: 1e9 }); }
}
// Context for the model within item_chat's bounds (40 messages, 16 KiB each, 64 KiB in total):
// the latest turns, oldest dropped first.
function chatContext(msgs) {
  const enc = new TextEncoder();
  const out = []; let total = 0;
  for (let i = msgs.length - 1; i >= 0 && out.length < 40; i -= 1) {
    let content = msgs[i].content;
    while (enc.encode(content).length > 16 * 1024 - 64) content = content.slice(0, Math.floor(content.length * 0.9));
    const size = enc.encode(content).length;
    if (total + size > 60 * 1024) break;
    total += size;
    out.unshift({ role: msgs[i].role, content });
  }
  return out;
}
async function askChat(id) {
  const c = chatFor(id);
  const q = c.local[c.local.length - 1];
  if (c.pending || !q || q.role !== 'user') return;
  c.pending = true; c.error = null; render();
  // One request id per question: a retry replays it instead of asking twice.
  q.rid ??= `mchat_${Date.now().toString(36)}_${crypto.getRandomValues(new Uint32Array(2)).join('')}`;
  try {
    const r = await source.itemChat({ id, requestId: q.rid, messages: chatContext(c.msgs.filter((m) => !m.dropped)) });
    c.local.push({ role: 'assistant', content: r.reply, time: chatTime(), model: r.model, cut: r.finish === 'length', rid: q.rid });
    noteOutcome(null);
  } catch (e) {
    noteOutcome(e);
    c.error = CHAT_ERR[e.code] || tx('对话失败（{0}）', e.code || 'error');
  }
  mergeHistory(c);
  c.pending = false;
  if (state.item?.id === id) render();
  document.getElementById('chat-scroll')?.scrollTo({ top: 1e9 });
  // Pick up the Mac's order (and any turn asked from the Mac meanwhile).
  if (!c.error && c.hist !== 'off') loadHistory(id);
}
// Folding hides the whole AI sheet (never the draft, the history or the article) and keeps the
// reading stats line, under a centered pill marked AI. The choice persists across items/reloads.
function setAiFolded(folded) { state.drawerOpen = !folded; localStorage.setItem('memex.m.aiFolded', folded ? '1' : '0'); render(); }
function renderDrawer() {
  const id = state.item?.id;
  const c = id ? chatFor(id) : { msgs: [], pending: false, error: null };
  const stats = DRAFTS && state.item?.meta ? h('div', { class: 'stats-strip', id: 'stats-strip', 'aria-label': tx('阅读统计') }) : null;
  const passBar = state.item?.meta ? psBar(state.item) : null;
  if (!state.drawerOpen) {
    const asked = c.msgs.filter((m) => m.role === 'user' && !m.dropped).length;
    return h('section', { class: 'drawer collapsed folded', 'aria-label': tx('对话') },
      h('button', { class: 'ai-pill', 'aria-expanded': 'false', 'aria-label': asked ? tx('展开 AI 对话（{0} 个提问）', asked) : tx('展开 AI 对话'), onclick: () => setAiFolded(false) },
        h('span', { class: 'ai-badge' }, 'AI'), h('span', { class: 'ai-pill-label' }, asked ? tx('对话 · {0}', asked) : tx('对话')),
        c.pending ? h('span', { class: 'spin', 'aria-hidden': 'true' }) : h('span', { class: 'ai-pill-up' }, icon('down', 14))),
      passBar, stats);
  }
  const ta = h('textarea', { rows: '1', placeholder: tx('就这篇文章提问…'), 'aria-label': tx('提问'), enterkeyhint: 'send' });
  ta.value = state.chatDraft;
  const send = h('button', { class: 'send', 'aria-label': tx('发送'), disabled: c.pending || !state.chatDraft.trim() }, icon('send', 18));
  ta.addEventListener('input', () => { state.chatDraft = ta.value; send.disabled = c.pending || !ta.value.trim(); ta.style.height = 'auto'; ta.style.height = `${Math.min(ta.scrollHeight, 120)}px`; });
  const submit = () => {
    const q = ta.value.trim();
    if (!q || c.pending || !id) return;
    // A question that failed and was not retried stays visible but is not sent again.
    const last = c.local[c.local.length - 1];
    if (last?.role === 'user') last.dropped = true;
    c.local.push({ role: 'user', content: withQuote(id, q), time: chatTime() });
    mergeHistory(c);
    state.chatDraft = '';
    askChat(id);
  };
  send.addEventListener('click', submit);
  const sample = (m) => h('div', { class: 'msg' },
    m.who === 'memex' ? assetImg({ class: 'av', src: 'assets/memex-avatar.png', alt: '' }) : h('span', { class: 'av' }, icon('user', 18)),
    h('div', {},
      m.who === 'memex' ? h('div', { class: 'who' }, ASSISTANT_NAME, h('span', { class: 'tag-demo' }, tx('示例'))) : null,
      h('div', { class: `bubble${m.who === 'memex' ? ' md-bubble' : ''}` }, m.who === 'memex' ? mdDoc(m.text, { breaks: true }) : m.text,
        m.refs ? h('div', { class: 'refs' }, m.refs.map((r) => h('button', { class: 'ref', onclick: () => toast(tx('引用跳转需连接 Mac 后可用')) }, `${r} ›`))) : null),
      h('div', { class: 'time' }, m.time)));
  const msg = (m) => h('div', { class: 'msg' },
    m.role === 'assistant' ? assetImg({ class: 'av', src: 'assets/memex-avatar.png', alt: '' }) : h('span', { class: 'av' }, icon('user', 18)),
    h('div', {},
      m.role === 'assistant' ? h('div', { class: 'who' }, ASSISTANT_NAME, GATEWAY ? null : h('span', { class: 'tag-demo' }, tx('示例'))) : null,
      m.restored ? h('span', { class: 'tag-restored' }, tx('从截图恢复')) : null,
      h('div', { class: `bubble${m.role === 'assistant' || m.restored ? ' md-bubble' : ''}` }, m.role === 'assistant' || m.restored ? mdDoc(m.content, { breaks: true }) : m.content, m.cut ? h('div', { class: 'time' }, m.stored ? tx('（内容过长，只显示前一部分）') : tx('（回答过长，已截断）')) : null),
      h('div', { class: 'time' }, m.time, m.dropped ? ` · ${tx('未发送成功')}` : '')));
  const storedCount = c.msgs.filter((m) => m.stored).length;
  const histLine = c.hist === 'loading' ? h('div', { class: 'hint hist' }, h('span', { class: 'spin' }), tx('正在读取 Mac 上保存的对话记录…'))
    : c.hist === 'err' ? h('div', { class: 'hint hist chat-error' }, tx('对话记录读取失败（{0}），新的提问不会带上之前的记录', reasonText(c.histErr)), ' ',
      h('button', { class: 'linkish', onclick: () => loadHistory(id) }, tx('重试')))
    : c.hist === 'cached' ? h('div', { class: 'hint hist' }, c.histErr ? tx('离线 · 缓存的对话记录') : tx('缓存的对话记录 · 正在向 Mac 确认…'))
    : c.hist === 'ok' && storedCount ? h('div', { class: 'hint hist' }, tx('已保存在 Mac 上的 {0} 条对话', String(storedCount)), c.partial ? ` · ${tx('记录太长，只载入了前一部分')}` : '')
    : null;
  const empty = c.hist === 'ok' ? tx('这篇还没有对话记录。可以就这篇文章提问，回答由 Mac 上设置的对话模型生成。')
    : tx('可以就这篇文章提问，回答由 Mac 上设置的对话模型生成。');
  const body = c.msgs.length
    ? [histLine, ...c.msgs.map(msg),
      c.pending ? h('div', { class: 'hint' }, tx('{0} 正在思考…', ASSISTANT_NAME)) : null,
      c.error ? h('div', { class: 'hint chat-error' }, c.error, ' ',
        h('button', { class: 'linkish', onclick: () => askChat(id) }, tx('重试'))) : null]
    : GATEWAY ? [histLine, c.hist === 'loading' || c.hist === 'err' ? null : h('div', { class: 'hint' }, empty)] : [histLine, ...SAMPLE_CHAT.map(sample)];

  return h('section', { class: 'drawer', 'aria-label': tx('对话') },
    h('button', { class: 'grab', 'aria-label': tx('收起对话'), 'aria-expanded': 'true', onclick: () => setAiFolded(true) }, h('span')),
    h('div', { class: 'drawer-head' }, tx('对话'), h('small', {}, GATEWAY ? tx('就这篇文章') : tx('示例 · 回答为合成内容')),
      h('button', { class: 'ai-fold', 'aria-label': tx('收起 AI'), onclick: () => setAiFolded(true) }, icon('down', 14), tx('收起 AI'))),
    h('div', { class: 'msgs', id: 'chat-scroll' }, body),
    h('div', { class: 'chips' },
      state.suggest ? [h('button', { class: 'facet', onclick: () => { ta.value = state.chatDraft = tx('总结这篇文章的核心论点'); send.disabled = c.pending; ta.focus(); } }, tx('总结核心论点')),
        h('button', { class: 'facet', onclick: () => { ta.value = state.chatDraft = tx('这篇文章的论据可靠吗？'); send.disabled = c.pending; ta.focus(); } }, tx('论据可靠吗'))] : null,
      h('span', { class: 'toggle' }, tx('建议提问'),
        h('button', { class: `switch${state.suggest ? ' on' : ''}`, role: 'switch', 'aria-checked': String(state.suggest), 'aria-label': tx('建议提问'), onclick: () => { state.suggest = !state.suggest; render(); } }))),
    quoteEl(id),
    h('div', { class: 'composer' }, ta, send),
    passBar, stats,
  );
}

// ---------- item info sheet (m1-3, m2-3 conflict) ----------
// Saves are revision CAS with an opaque revision_id. A timeout/disconnect means the outcome is
// unknown: read back before deciding, never blindly resend.
function openSheet() {
  const m = state.item.meta;
  state.sheet = { id: m.item_id, revision: m.revision_id, base: { title: m.title, tags: [...m.manual_tags] }, draft: { title: m.title, tags: [...m.manual_tags] }, saving: false, conflict: false };
  render();
}
async function reloadIntoSheet() {
  const s = state.sheet;
  try {
    const m = withRow(await source.itemRead(s.id));
    state.item.meta = m;
    state.sheet = { id: m.item_id, revision: m.revision_id, base: { title: m.title, tags: [...m.manual_tags] }, draft: { title: m.title, tags: [...m.manual_tags] }, saving: false, conflict: false };
    noteOutcome(null);
  } catch (e) { noteOutcome(e); toast(ERR_TEXT[e.code] || tx('重新载入失败')); }
  render();
}
// Outcome unknown: read the item back. If the new revision carries our draft, it was saved;
// otherwise keep the draft so the user can decide. Never resend automatically.
async function readBackUnknown() {
  const s = state.sheet;
  try {
    const m = withRow(await source.itemRead(s.id));
    noteOutcome(null);
    state.item.meta = m;
    const applied = m.revision_id !== s.revision && m.title === (s.draft.title.trim() || s.base.title)
      && m.manual_tags.join('\u0001') === s.draft.tags.join('\u0001');
    if (applied) { state.sheet = null; state.lib.view = null; render(); toast(tx('读回确认：已保存')); return; }
    if (m.revision_id !== s.revision) { s.unknown = false; s.conflict = true; render(); return; }
    s.unknown = false; render(); toast(tx('读回确认：未保存，修改仍在，可再次保存'));
  } catch (e) { noteOutcome(e); render(); }
}
function closeSheet() {
  if (!state.sheet) return;
  if (isDirty() && !state.sheet.conflict && !confirm(tx('有未保存的修改，确定放弃吗？'))) return;
  state.sheet = null; render();
}
function isDirty() {
  const s = state.sheet; if (!s) return false;
  return s.draft.title !== s.base.title || s.draft.tags.join('\u0001') !== s.base.tags.join('\u0001');
}

function renderSheet() {
  const s = state.sheet;
  const m = state.item.meta;

  const title = h('textarea', { rows: '2', 'aria-label': tx('标题'), disabled: s.conflict || s.saving });
  title.value = s.draft.title;
  title.addEventListener('input', () => { s.draft.title = title.value; footState(); });

  const tagWrap = h('div', { class: 'tags' });
  const drawTags = () => {
    const add = h('input', { class: 'tag-add', placeholder: tx('＋ 添加'), 'aria-label': tx('添加标签'), enterkeyhint: 'done', disabled: s.conflict || s.saving });
    add.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.isComposing) {
        const v = add.value.trim();
        if (v && !s.draft.tags.includes(v)) s.draft.tags.push(v);
        add.value = ''; drawTags(); footState(); tagWrap.querySelector('.tag-add').focus();
      }
    });
    tagWrap.replaceChildren(...s.draft.tags.map((t) => h('span', { class: 'tagc' }, t,
      s.conflict ? null : h('button', { 'aria-label': tx('移除标签 {0}', t), onclick: () => { s.draft.tags = s.draft.tags.filter((x) => x !== t); drawTags(); footState(); } }, icon('x', 14)))), add);
  };
  drawTags();

  const tr = { done: [tx('已完成'), ''], running: [tx('进行中'), ' run'], none: [tx('未翻译'), ' off'] }[m.translation_status] || [tx('暂不可用'), ' off'];
  const stateEl = h('span', { class: 'state' });
  const saveBtn = h('button', { class: 'btn primary', onclick: save }, s.saving ? tx('保存中…') : tx('保存'));
  function footState() {
    const d = isDirty();
    stateEl.textContent = s.conflict ? '' : d ? tx('有未保存修改') : tx('没有修改');
    stateEl.className = `state${d ? '' : ' quiet'}`;
    saveBtn.disabled = !d || s.conflict || s.saving || s.unknown;
  }
  async function save() {
    s.saving = true; render();
    const draft = { title: s.draft.title.trim() || s.base.title, tags: [...s.draft.tags] };
    try {
      const r = await source.updateItemMetadata({ item_id: s.id, expected_revision_id: s.revision, title: draft.title, manual_tags: draft.tags });
      noteOutcome(null);
      Object.assign(state.item.meta, { title: draft.title, manual_tags: draft.tags, revision_id: r.revision_id });
      state.item.noCache = true; cacheDrop(s.id); // the next open reads the new revision from the Mac
      cacheKv('library', null); // cached rows carry the old title
      state.lib.view = null; // library rows may be stale now; next visit re-queries from the first window
      state.sheet = null; render();
      toast(source.kind === 'demo' ? tx('演示：只保存在本页，未写入 Mac') : tx('已保存'));
    } catch (e) {
      s.saving = false;
      if (e.code === 'stale_item_revision') { s.conflict = true; noteOutcome(null); }
      else if (e.code === 'unavailable') { noteOutcome(e); toast(tx('未连接到 Mac，修改尚未保存')); }
      else { s.unknown = true; state.item.noCache = true; cacheDrop(s.id); cacheKv('library', null); render(); return readBackUnknown(); }
      render();
    }
  }

  const body = h('div', { class: 'body' },
    s.unknown ? h('div', { class: 'banner warn in-sheet' }, icon('warn'),
      h('span', { class: 'grow' }, tx('保存结果未知，正在向 Mac 读回确认。')),
      h('button', { class: 'btn-sm', onclick: readBackUnknown }, tx('重新确认'))) : null,
    s.conflict ? h('div', { class: 'banner warn in-sheet' }, icon('warn'),
      h('span', { class: 'grow' }, tx('此条目已在 Mac 上更新，当前修改无法保存。请取消并重新载入。'))) : null,
    h('div', { class: 'field' }, h('div', { class: 'k' }, tx('标题')), title),
    h('div', { class: 'field inline' }, h('div', { class: 'k' }, tx('类型')), h('div', { class: 'v ro' }, m.item_type || tx('未知'))),
    h('div', { class: 'field inline' }, h('div', { class: 'k' }, tx('来源')), h('div', { class: 'v ro' }, showSource(m.source))),
    h('div', { class: 'field inline' }, h('div', { class: 'k' }, tx('添加日期')), h('div', { class: 'v ro' }, showDate(m.captured_at, 16))),
    m.translation_status == null ? null : h('div', { class: 'field inline' }, h('div', { class: 'k' }, tx('翻译状态')), h('span', { class: 'pill' }, h('span', { class: `dot${tr[1]}` }), tr[0])),
    h('div', { class: 'field' }, h('div', { class: 'k' }, tx('标签')), tagWrap),
    m.summary ? h('div', { class: 'field' }, h('div', { class: 'k' }, tx('摘要（分析结果，只读）')), h('div', { class: 'ro-box' }, m.summary)) : null,
  );

  const foot = s.conflict
    ? h('div', { class: 'sheet-foot' },
      h('button', { class: 'btn primary grow1', onclick: reloadIntoSheet }, tx('取消并重新载入')),
      h('button', { class: 'btn grow1', disabled: true }, tx('保存')))
    : h('div', { class: 'sheet-foot' }, stateEl, h('button', { class: 'btn', disabled: s.saving, onclick: closeSheet }, tx('取消')), saveBtn);
  footState();

  return [
    h('div', { class: 'scrim', onclick: () => { if (!s.saving) closeSheet(); } }),
    h('section', { class: 'sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('条目信息') },
      h('div', { class: 'grab' }, h('span')), h('h2', {}, tx('条目信息')), body, foot),
  ];
}
// ---------- other tabs ----------
// ---------- 对话 tab: paste a link, the Mac captures it (m1 chat language: bottom input, MemeX bubbles) ----------
// Each link becomes one capture job with its own request_key. Only {request_key, url, job_id,
// item_id, states} are kept on the phone (no article text); the job itself runs on the Mac.
const TASK_KEY = GATEWAY ? 'memex.m.gw.tasks' : 'memex.m.demo.tasks';
// Persisted projection: only the task reference and its states. The pasted text, note messages,
// item titles and any article text stay in memory and are never written to storage.
const PERSIST = ['request_key', 'url', 'job_id', 'item_id', 'capture', 'capture_reason', 'translation', 'reason'];
const project = (t) => Object.fromEntries(PERSIST.map((k) => [k, t[k] ?? null]));
function loadTasks() {
  let v;
  try { v = JSON.parse(localStorage.getItem(GATEWAY ? 'memex.m.gw.tasks' : 'memex.m.demo.tasks') || '[]'); } catch { v = []; }
  // Older records may carry text/title/at: keep the task reference, drop those fields (rewritten on load).
  const tasks = (Array.isArray(v) ? v : [])
    .filter((t) => t && typeof t === 'object' && typeof t.request_key === 'string' && typeof t.url === 'string' && (t.kind ?? 'capture') === 'capture')
    .slice(-30).map((t) => ({ kind: 'capture', ...project(t), error: null }));
  localStorage.setItem(GATEWAY ? 'memex.m.gw.tasks' : 'memex.m.demo.tasks', JSON.stringify(tasks.map(project)));
  return tasks;
}
function saveTasks() { localStorage.setItem(TASK_KEY, JSON.stringify(state.tasks.filter((t) => t.kind === 'capture' && t.error !== 'invalid').slice(-30).map(project))); }
const URL_RE = /https:\/\/[^\s<>"'，。；、）)】]+/g;
function newRequestKey() {
  const b = crypto.getRandomValues(new Uint8Array(24));
  return `remote_mobile_${btoa(String.fromCharCode(...b)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`;
}
// lost/denied/conflict are local terminal states: the Mac refused or no longer knows this task.
const CAPTURE_DONE = new Set(['succeeded', 'failed', 'cancelled', 'lost', 'denied', 'conflict']);
const CAPTURE_FATAL = { remote_resource_not_found: 'lost', permission_denied: 'denied', request_identity_conflict: 'conflict', invalid_request: 'denied' };
const TR_DONE = new Set(['succeeded', 'failed', 'skipped', 'not_requested', 'unavailable']);
const taskDone = (t) => t.error === 'invalid' || (CAPTURE_DONE.has(t.capture) && (t.capture !== 'succeeded' || TR_DONE.has(t.translation)));

function submitAgent(text) {
  const urls = [...new Set((text.match(URL_RE) || []).map((u) => u.replace(/[.,!?]+$/, '')))].slice(0, 5);
  const at = new Date().toTimeString().slice(0, 5);
  if (!urls.length) {
    state.tasks.push({ kind: 'note', text, at });
  } else {
    for (const url of urls) {
      let valid = true;
      try { const u = new URL(url); valid = u.protocol === 'https:' && !u.username && !u.password; } catch { valid = false; }
      state.tasks.push({ kind: 'capture', text: urls.length > 1 ? url : text, url, at, request_key: newRequestKey(),
        job_id: null, item_id: null, title: null, capture: valid ? 'sending' : null, translation: null, reason: null, error: valid ? null : 'invalid' });
    }
  }
  saveTasks(); render(); pollTasks();
}

let pollTimer = null;
let polling = false;
async function pollTasks() {
  clearTimeout(pollTimer);
  if (polling || !state.paired || !source) return;
  polling = true;
  let changed = false;
  for (const t of state.tasks) {
    if (t.kind !== 'capture') continue;
    // Finished tasks restored from storage only need their title again (kept in memory only).
    if (taskDone(t) && (t.title || !t.item_id)) continue;
    try {
      if (!CAPTURE_DONE.has(t.capture)) {
        // No job id yet (never sent, or outcome unknown): resend with the same request_key.
        const j = t.job_id ? await source.captureStatus({ job_id: t.job_id }) : await source.captureEnqueue({ request_key: t.request_key, url: t.url });
        if (t.capture !== j.state || t.job_id !== j.job_id) changed = true;
        Object.assign(t, { job_id: j.job_id, capture: j.state, capture_reason: typeof j.reason_code === 'string' ? j.reason_code : null, item_id: j.item_id ?? t.item_id, error: null });
        if (j.state === 'succeeded') { state.lib.view = null; }
      }
      if (t.capture === 'succeeded' && t.item_id) {
        if (!t.title) { const m = await source.itemRead(t.item_id); t.title = m.title; changed = true; }
        if (TR_DONE.has(t.translation)) { noteOutcome(null); continue; }
        try {
          const tr = await source.translationStatus(t.item_id);
          // awaiting_enqueue (post-Store hook pending) is non-terminal; not_requested is final.
          if (t.translation !== tr.state) changed = true;
          Object.assign(t, { translation: tr.state, reason: tr.reason ?? null });
        } catch (e) {
          if (e.code === 'unavailable') throw e;
          t.translation = 'unavailable'; changed = true; // gateway has no translation status yet
        }
      }
      noteOutcome(null);
    } catch (e) {
      // Refused or unknown to this pairing: stop. Never mint a new key and never resend.
      if (!CAPTURE_DONE.has(t.capture) && CAPTURE_FATAL[e.code]) { t.capture = CAPTURE_FATAL[e.code]; t.error = null; changed = true; continue; }
      // Not reachable / outcome unknown: keep the task; the next poll resends the same key and URL.
      if (t.error !== (e.code || 'unknown')) changed = true;
      t.error = e.code || 'unknown';
      if (e.code === 'unavailable') { noteOutcome(e); break; }
    }
  }
  polling = false;
  if (changed) { saveTasks(); if (['chat', 'tasks'].includes(route().name)) render(); }
  if (state.tasks.some((t) => t.kind === 'capture' && (!taskDone(t) || (t.item_id && !t.title && !['lost', 'denied', 'conflict'].includes(t.capture))))) pollTimer = setTimeout(pollTasks, GATEWAY ? 3000 : 1200);
}

// Known reasons of a stopped or waiting job (fixed machine codes); others fall back to the state text.
const CAPTURE_REASON = {
  challenge_detected: tx('网站要求人机验证，没抓到正文'), interactive_login_required: tx('网站需要登录，没抓到正文'),
  weixin_source_conflict: tx('同一篇公众号文章已有不同来源，请在 Mac 上核对'), weixin_content_invalid: tx('公众号正文或图片不完整，抓取失败'),
  weixin_commit_retry: tx('存入资料库没成功，稍后自动重试'), retry_exhausted: tx('多次重试仍未成功'),
  address_not_public: tx('这个地址不允许抓取'), network_policy_rejected: tx('这个地址不允许抓取'), unsupported_content: tx('不支持这种网页内容'),
};
const captureText = (t) => (['needs_user', 'failed', 'retry_wait'].includes(t.capture) && CAPTURE_REASON[t.capture_reason]) || CAPTURE_TEXT[t.capture];
const CAPTURE_TEXT = {
  sending: tx('正在发送给 Mac…'), pending_delivery: tx('正在等待 Mac 接收…'), queued: tx('Mac 已接收，正在排队'),
  leased: tx('正在抓取网页正文和元数据'), artifact_ready: tx('网页已抓取，正在存入资料库'), retry_wait: tx('稍后自动重试抓取'),
  needs_user: tx('需要在 Mac 上批准后继续'), succeeded: tx('已存入资料库'), failed: tx('抓取失败'), cancelled: tx('已取消'),
  lost: tx('属于之前的配对，已无法查询'), denied: tx('当前连接没有收录权限'), conflict: tx('同一任务对应了不同链接，已停止'),
};
const SKIP_TEXT = { already_chinese: tx('中文原文，无需翻译'), challenge_or_login: tx('页面需要登录或验证，未翻译'), source_language_unknown: tx('无法判断语言，未翻译'),
  source_too_large: tx('原文过长，未自动翻译'), unsupported_item_type: tx('此类条目不翻译'), stale_revision: tx('条目已更新，未翻译') };
function stepsFor(t) {
  const c = t.capture;
  const fetchState = ['leased'].includes(c) ? 'run' : ['artifact_ready', 'succeeded'].includes(c) ? 'ok' : ['failed', 'cancelled', 'lost', 'denied', 'conflict'].includes(c) ? 'bad' : 'wait';
  const storeState = c === 'succeeded' ? 'ok' : c === 'artifact_ready' ? 'run' : 'wait';
  const tr = t.translation;
  const trState = c !== 'succeeded' ? 'wait' : tr === 'succeeded' ? 'ok' : ['skipped', 'not_requested', 'unavailable'].includes(tr) ? 'skip' : tr === 'failed' ? 'bad' : 'run';
  const trText = tr === 'succeeded' ? tx('已翻译成中文') : tr === 'skipped' ? (SKIP_TEXT[t.reason] || tx('未翻译')) : tr === 'failed' ? tx('翻译失败，原文仍可读')
    : tr === 'unavailable' ? tx('翻译状态暂不可用') : tr === 'not_requested' ? tx('没有排队翻译') : tr === 'awaiting_enqueue' ? tx('等待翻译排队') : tr === 'running' ? tx('正在翻译成中文') : c === 'succeeded' ? tx('等待翻译') : tx('翻译（英文文章）');
  return [
    [['sending', 'pending_delivery'].includes(c) ? 'run' : c ? 'ok' : 'wait', ['sending', 'pending_delivery', 'queued'].includes(c) ? CAPTURE_TEXT[c] : tx('Mac 已接收')],
    [fetchState, ['leased', 'retry_wait', 'needs_user', 'failed', 'cancelled', 'lost', 'denied', 'conflict'].includes(c) ? captureText(t) : tx('抓取正文和元数据')],
    [storeState, c === 'succeeded' ? tx('已存入资料库') : c === 'artifact_ready' ? CAPTURE_TEXT[c] : tx('存入资料库')],
    [trState, trText],
  ];
}
const STEP_ICON = { ok: '✓', run: '', bad: '!', skip: '–', wait: '' };

function memexBubble(...kids) {
  return h('div', { class: 'msg' }, assetImg({ class: 'av', src: 'assets/memex-avatar.png', alt: '' }),
    h('div', {}, h('div', { class: 'who' }, ASSISTANT_NAME, GATEWAY ? null : h('span', { class: 'tag-demo' }, tx('演示'))), h('div', { class: 'bubble agent' }, ...kids)));
}
function userBubble(text, at) {
  return h('div', { class: 'msg me' }, h('div', {}, h('div', { class: 'bubble mine' }, text), h('div', { class: 'time' }, at)));
}
function taskBubble(t) {
  if (t.error === 'invalid') return memexBubble(tx('这个链接无法收录：只支持 https 开头的网页链接。'));
  const done = t.capture === 'succeeded' && TR_DONE.has(t.translation);
  const open = () => { state.openLang = t.translation === 'succeeded' ? 'trans' : 'orig'; state.item = null; go(`/item/${t.item_id}`); };
  return memexBubble(
    h('div', { class: 'cap-url' }, t.url.replace(/^https:\/\//, '')),
    h('ol', { class: 'steps' }, stepsFor(t).map(([st, label]) => h('li', { class: `step ${st}` }, h('span', { class: 'mark' }, STEP_ICON[st]), label))),
    t.error && t.error !== 'invalid' ? h('div', { class: 'cap-note' }, t.error === 'unavailable' ? tx('暂时连不上 Mac，恢复后自动继续。') : tx('结果未知，正在向 Mac 确认，不会重复收录。')) : null,
    t.item_id && t.title ? h('button', { class: 'cap-card', onclick: open },
      h('span', { class: 'doc' }, icon('doc', 22)),
      h('span', { class: 'main' }, h('span', { class: 't serif' }, t.title), h('span', { class: 'm' }, done ? (t.translation === 'succeeded' ? tx('打开阅读 · 已有中文译文') : tx('打开阅读')) : tx('可以先读原文'))),
      icon('chev', 16)) : null,
  );
}

function renderChatTab() {
  const list = h('div', { class: 'scroll agent-log', id: 'agent-scroll' },
    memexBubble(tx('把文章链接发给我，我会在你的 Mac 上抓取正文、采集元数据，英文文章再翻译成中文，完成后在这里直接打开阅读。'),
      h('div', { class: 'cap-note' }, tx('就文章提问的对话还没有接通，目前只能收录链接。'))),
    state.tasks.flatMap((t) => [userBubble(t.text ?? t.url, t.at ?? ''), t.kind === 'note' ? memexBubble(tx('我目前只能收录网页链接。请粘贴以 https:// 开头的文章链接。')) : taskBubble(t)]));
  const ta = h('textarea', { rows: '1', placeholder: tx('粘贴文章链接…'), 'aria-label': tx('粘贴文章链接'), enterkeyhint: 'send', autocapitalize: 'off', autocomplete: 'off' });
  ta.value = state.agentDraft;
  const send = h('button', { class: 'send', 'aria-label': tx('发送'), disabled: !state.agentDraft.trim() }, icon('send', 18));
  const grow = () => { ta.style.height = 'auto'; ta.style.height = `${Math.min(ta.scrollHeight, 140)}px`; };
  ta.addEventListener('input', () => { state.agentDraft = ta.value; send.disabled = !ta.value.trim(); grow(); });
  const go_ = () => { const v = ta.value.trim(); if (!v) return; state.agentDraft = ''; submitAgent(v); };
  send.addEventListener('click', go_);
  ta.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); go_(); } });
  requestAnimationFrame(() => { list.scrollTop = list.scrollHeight; grow(); });
  return h('div', { class: 'screen' }, ribbon(),
    h('header', { class: 'top' }, h('div', { class: 'brand serif' }, BRAND_NAME, h('span', { class: 'brand-ver' }, `v${APP_VERSION}`)), connChip()),
    list,
    h('div', { class: 'composer agent-composer' }, ta, send),
    tabbar('chat'));
}

function renderTasks() {
  const caps = state.tasks.filter((t) => t.kind === 'capture' && t.error !== 'invalid').slice().reverse();
  const label = (t) => (t.capture === 'succeeded' ? stepsFor(t)[3][1] : captureText(t) || tx('等待中'));
  return h('div', { class: 'screen' }, ribbon(), h('div', { class: 'page-title serif' }, tx('后台任务')),
    h('div', { class: 'scroll' },
      caps.length ? h('div', { class: 'group' }, caps.map((t) => h('button', { class: 'cell', onclick: () => (t.item_id ? go(`/item/${t.item_id}`) : go('/chat')) },
        h('div', { class: 'grow' }, h('div', {}, t.title || t.url.replace(/^https:\/\//, '')), h('div', { class: 'sub' }, t.at ? tx('收录 · {0}', t.at) : tx('收录'))),
        h('span', { class: `task-state${taskDone(t) ? '' : ' run'}` }, label(t)))))
        : h('div', { class: 'empty' }, tx('没有收录任务。'), h('br'), tx('在“对话”里粘贴文章链接即可开始。'))),
    tabbar('tasks'));
}
function renderSettings() {
  const seg = h('div', { class: 'seg3' }, [['system', tx('跟随系统')], ['light', tx('浅色')], ['dark', tx('深色')]].map(([k, label]) =>
    h('button', { class: state.theme === k ? 'on' : '', onclick: () => { state.theme = k; localStorage.setItem('memex.m.theme', k); applyTheme(); render(); } }, label)));
  const sw = (on, label, fn) => h('button', { class: `switch${on ? ' on' : ''}`, role: 'switch', 'aria-checked': String(on), 'aria-label': label, onclick: fn });
  const appearance = [
    h('div', { class: 'group-title' }, tx('外观')),
    h('div', { class: 'group' }, h('div', { class: 'cell' }, h('span', { class: 'grow' }, tx('日夜模式')), seg)),
    h('div', { class: 'group' }, h('div', { class: 'cell' }, h('span', { class: 'grow' }, tx('界面语言')),
      h('div', { class: 'seg3' }, [['system', tx('跟随系统')], ['zh', '中文'], ['en', 'English']].map(([k, label]) =>
        h('button', { class: langPref() === k ? 'on' : '', onclick: () => { if (langPref() !== k) setLangPref(k); } }, label))))),
    h('div', { class: 'group' }, h('div', { class: 'cell' }, h('span', { class: 'grow' }, tx('注释字体')),
      h('div', { class: 'seg3 note-font', role: 'group', 'aria-label': tx('注释字体') }, [['kai', tx('楷体')], ['song', tx('宋体')], ['body', tx('与正文相同')]].map(([k, label]) =>
        h('button', { class: `nf-${k}${state.noteFont === k ? ' on' : ''}`, 'aria-pressed': String(state.noteFont === k), 'data-font': k,
          onclick: () => { state.noteFont = k; localStorage.setItem('memex.m.noteFont', k); applyNoteFont(); render(); } }, label))))),
    h('div', { class: 'group' }, h('div', { class: 'cell spk-cell' }, h('div', { class: 'grow' }, h('div', {}, tx('说话人名字')), h('div', { class: 'sub' }, tx('逐字稿段首的名字和「正在说」气泡；中文名只作参考显示，不改正文。没有可靠对照时显示原有标签'))),
      h('div', { class: 'seg3 spk-name-seg', role: 'group', 'aria-label': tx('说话人名字') }, [['orig', tx('原文名')], ['trans', tx('中文参考名')], ['both', tx('原文名（中文名）')]].map(([k, label]) =>
        h('button', { class: state.spkName === k ? 'on' : '', 'aria-pressed': String(state.spkName === k), 'data-spk-opt': k,
          onclick: () => { state.spkName = k; localStorage.setItem('memex.m.spkName', k); render(); } }, label))))),
    h('div', { class: 'group' }, h('div', { class: 'cell' },
      h('div', { class: 'grow' }, h('div', {}, tx('锁定竖屏')), h('div', { class: 'sub' }, tx('网页无法关闭系统旋转，开启后横放手机时界面保持竖直；也可使用 iPhone 控制中心的竖屏锁定'))),
      sw(lockPortrait(), tx('锁定竖屏'), () => setLockPortrait(!lockPortrait())))),
  ];
  const general = [
    h('div', { class: 'group-title' }, tx('连接')),
    h('div', { class: 'group' },
      h('div', { class: 'cell' }, h('div', { class: 'grow' }, h('div', {}, tx('我的 Mac')), h('div', { class: 'sub' }, GATEWAY ? tx('经端到端加密中继连接，无需 VPN') : tx('演示连接 · 正式版经加密中继连接你的 Mac，无需 VPN'))), h('span', { class: `dot${state.online ? '' : ' off'}` })),
      GATEWAY ? h('button', { class: 'cell', onclick: async () => { if (!confirm(tx('重新配对后，之前配对发起的收录任务将无法再查询。继续吗？'))) return; await conn.unpair(); await cacheDestroy(); capsP = null; state.caps = null; state.chats = {}; state.item = null; state.groups = null; state.lib.rows = []; state.paired = false; go('/'); } }, h('span', { class: 'grow' }, tx('重新配对')), icon('chev', 16))
        : h('button', { class: 'cell', onclick: () => { state.paired = false; localStorage.removeItem('memex.m.paired'); go('/'); } }, h('span', { class: 'grow' }, tx('查看配对页')), icon('chev', 16))),
    h('div', { class: 'group-title' }, tx('离线缓存')),
    h('div', { class: 'group' }, h('div', { class: 'cell' },
      h('div', { class: 'grow' }, h('div', {}, tx('最近打开的文章')),
        h('div', { class: 'sub', id: 'cache-count' }, tx('最多保留 {0} 篇，只存在这个网页里', CACHE_ITEMS))),
      h('button', { class: 'btn-sm', onclick: async () => { await cacheClear(); state.item = null; state.chats = {}; paintCacheCount(); toast(tx('已清除缓存')); } }, tx('清除缓存')))),
    GATEWAY ? null : h('div', { class: 'group-title' }, tx('审稿用：模拟状态')),
    GATEWAY ? null : h('div', { class: 'group' },
      h('div', { class: 'cell' }, h('div', { class: 'grow' }, h('div', {}, tx('模拟离线')), h('div', { class: 'sub' }, tx('显示“无法连接到你的 Mac”'))),
        sw(source.flags.offline, tx('模拟离线'), () => { source.flags.offline = !source.flags.offline; Object.assign(state.lib, { view: null, error: null }); render(); })),
      h('div', { class: 'cell' }, h('div', { class: 'grow' }, h('div', {}, tx('下次保存时模拟冲突')), h('div', { class: 'sub' }, tx('模拟条目已在 Mac 上被修改'))),
        sw(source.flags.conflictNextSave, tx('模拟冲突'), () => { source.flags.conflictNextSave = !source.flags.conflictNextSave; render(); })),
      h('div', { class: 'cell' }, h('div', { class: 'grow' }, h('div', {}, tx('下次保存时模拟结果未知')), h('div', { class: 'sub' }, tx('Mac 已写入但回复丢失，需读回确认'))),
        sw(source.flags.unknownNextSave, tx('模拟结果未知'), () => { source.flags.unknownNextSave = !source.flags.unknownNextSave; render(); }))),
    GATEWAY ? null : h('div', { class: 'small foot-note' }, tx('这是手机端演示版（m1/m2）。资料是合成示例，不来自你的 Mac；保存只在本页生效。')),
  ];
  if (!DRAFTS) return h('div', { class: 'screen' }, ribbon(), h('div', { class: 'page-title serif' }, tx('设置')), h('div', { class: 'scroll' }, ...appearance, ...general), tabbar('settings'));
  // Draft: tabs like the desktop settings (通用 / 外观 / 版本; 模型 stays on the Mac).
  const tab = state.setTab || 'general';
  const tabs = h('div', { class: 'set-tabs', role: 'tablist' }, [['general', tx('通用')], ['appearance', tx('外观')], ['version', tx('版本')], ['changelog', tx('更新日志')]].map(([k, label]) =>
    h('button', { class: tab === k ? 'on' : '', role: 'tab', 'aria-selected': String(tab === k), onclick: () => { state.setTab = k; render(); } }, label)));
  const body = tab === 'appearance' ? [...appearance, ...statsSettings()] : tab === 'version' ? versionPage() : tab === 'changelog' ? changelogPage() : general;
  return h('div', { class: 'screen' }, ribbon(), h('div', { class: 'page-title serif' }, tx('设置')), tabs, h('div', { class: 'scroll' }, ...body), tabbar('settings'));
}
// 外观 › 阅读统计 (draft): what the reader's stats strip counts and how fast it assumes you read.
function statsSettings() {
  const p = statsPrefs();
  const pick = (key, opts) => h('div', { class: 'seg3' }, opts.map(([v, label]) => h('button', { class: p[key] === v ? 'on' : '', onclick: () => setStatsPref(key, v) }, label)));
  const speed = (key, opts, unit) => {
    const sel = h('select', { class: 'speed', 'aria-label': unit }, opts.map((v) => h('option', { value: String(v), selected: p[key] === v }, `${v} ${unit}`)));
    sel.addEventListener('change', () => setStatsPref(key, Number(sel.value)));
    return sel;
  };
  const sample = countText(`${ASSISTANT_NAME} 是一个**本地优先**的资料库，支持 retrieval-augmented reading。`, p);
  return [
    h('div', { class: 'group-title' }, tx('阅读统计')),
    h('div', { class: 'group' },
      h('div', { class: 'cell col' }, h('div', { class: 'grow' }, h('div', {}, tx('字数统计方式')),
        h('div', { class: 'sub' }, p.method === 'mixed' ? tx('中文按字计（含标点），英文按词计（不含标点）') : tx('所有非空白字符逐个计数'))),
      pick('method', [['mixed', tx('中文字 + 英文词')], ['chars', tx('逐字符')]])),
      h('div', { class: 'cell' }, h('div', { class: 'grow' }, h('div', {}, tx('统计 Markdown 标记')),
        h('div', { class: 'sub' }, tx('关闭时不计 #、**、链接地址、表格竖线等标记'))),
      h('button', { class: `switch${p.markup ? ' on' : ''}`, role: 'switch', 'aria-checked': String(p.markup), 'aria-label': tx('统计 Markdown 标记'), onclick: () => setStatsPref('markup', !p.markup) })),
      h('div', { class: 'cell' }, h('span', { class: 'grow' }, tx('中文阅读速度')), speed('zh', [250, 300, 350, 400, 500, 600, 800], tx('字/分钟'))),
      h('div', { class: 'cell' }, h('span', { class: 'grow' }, tx('英文阅读速度')), speed('en', [150, 200, 230, 250, 300, 400], tx('词/分钟')))),
    h('div', { class: 'small foot-note' }, tx('示例：「{0}」按当前设置计为 {1}。', `${ASSISTANT_NAME} 是一个**本地优先**的资料库，支持 retrieval-augmented reading。`,
      p.method === 'chars' ? tx('{0} 字符', sample.cjk) : `${tx('{0} 字', sample.cjk)} + ${tx('{0} 词', sample.words)}`)),
  ];
}
// 版本 (draft): the portrait, this page's version and bundle, and what the Mac reports.
function versionPage() {
  if (!state.caps && !state.capsErr) caps().then(() => { if (route().name === 'settings') render(); }, (e) => { state.capsErr = e.code || 'unknown'; if (route().name === 'settings') render(); });
  const v = state.caps?.versions;
  const unknown = (why) => h('span', { class: 'muted' }, why);
  const row = (label, value, mono) => h('div', { class: 'cell vrow' }, h('span', { class: 'k' }, label), h('span', { class: `v${mono ? ' mono' : ''}` }, value));
  const mac = () => (!GATEWAY ? unknown(tx('演示，未连接 Mac')) : state.capsErr ? unknown(tx('读取失败（{0}）', reasonText(state.capsErr))) : !state.caps ? unknown(tx('读取中…')) : null);
  const fromMac = (val, missing) => mac() || (val ? val : unknown(missing));
  return [
    h('div', { class: 'ver-head' }, assetImg({ class: 'ver-avatar', src: 'assets/memex-avatar.png', alt: tx('黑玫瑰形象') }),
      h('div', {}, h('div', { class: 'ver-name serif' }, BRAND_NAME), h('div', { class: 'ver-num' }, tx('版本 {0}', APP_VERSION)),
        h('div', { class: 'sub' }, GATEWAY ? tx('手机网页版 · 由你的 Mac 提供') : tx('手机网页版 · 演示预览')))),
    h('div', { class: 'group-title' }, tx('本应用')),
    h('div', { class: 'group' },
      row(tx('应用版本'), APP_VERSION, true),
      row(tx('网页包'), GATEWAY ? fromMac(v?.web_bundle, tx('网关未提供')) : unknown(tx('演示预览，不经网关提供')), true),
      row(tx('构建日期'), BUILD_DATE, false)),
    h('div', { class: 'group-title' }, tx('我的 Mac')),
    h('div', { class: 'group' },
      // agent null: the Mac did not report it (unreachable or older); binary_sha256 null: still hashing.
      row(tx('代理版本'), fromMac(v?.agent?.version, v ? tx('未知（代理未报告）') : tx('网关未提供')), true),
      row(tx('代理程序哈希'), fromMac(v?.agent?.binary_sha256, !v ? tx('网关未提供') : v.agent ? tx('计算中…') : tx('未知（代理未报告）')), true),
      row(tx('网关版本'), fromMac(v?.gateway, tx('网关未提供')), true)),
    h('div', { class: 'small foot-note' }, tx('哈希是构建标识（文件内容摘要的前 12 位），不含任何密钥或路径。')),
  ];
}

// 更新日志: the portrait hero (about half the screen) over the releases, newest first.
function changelogPage() {
  if (!CHANGELOG) {
    import('./changelog.mjs').then((m) => { CHANGELOG = m.default; if (state.setTab === 'changelog') render(); },
      () => { CHANGELOG = { releases: [] }; if (state.setTab === 'changelog') render(); });
    return [h('div', { class: 'small foot-note' }, tx('正在读取更新日志…'))];
  }
  if (!CHANGELOG.releases.length) return [h('div', { class: 'small foot-note' }, tx('当前版本 {0}', APP_VERSION))];
  const where = { mac: 'Mac', phone: tx('手机') };
  const [latest] = CHANGELOG.releases;
  return [
    h('div', { class: 'log-hero' },
      assetImg({ class: 'log-portrait', src: 'assets/memex-portrait.png', alt: tx('黑玫瑰形象') }),
      h('div', { class: 'log-veil' }),
      h('div', { class: 'log-title' },
        h('div', { class: 'log-name serif' }, BRAND_NAME),
        h('div', { class: 'log-motto serif' }, tx('为你珍藏每一篇值得读的文字')),
        h('div', { class: 'log-badge' }, `v${latest.version}`, h('span', {}, tx('共 {0} 次更新', CHANGELOG.releases.length))))),
    h('ol', { class: 'log-list' }, CHANGELOG.releases.map((r, i) => h('li', { class: `log-rel${i === 0 ? ' now' : ''}` },
      h('div', { class: 'log-rel-head' },
        h('span', { class: 'log-ver serif' }, `v${r.version}`),
        i === 0 ? h('span', { class: 'log-cur' }, tx('当前版本')) : null,
        h('span', { class: 'log-meta' }, `${r.date} · ${r.platforms.map((x) => where[x]).join(' · ')}`)),
      h('ul', {}, r.items.map((t) => h('li', {}, t)))))),
  ];
}

async function paintCacheCount() {
  const n = await cacheCount();
  const el = document.getElementById('cache-count');
  if (el) el.textContent = tx('已缓存 {0} 篇（最多 {1} 篇），只存在这个网页里', n, CACHE_ITEMS);
}


// ---------- reader chrome (drafts): highlight scrubber + reading stats ----------
// Stats count the item's original text: CJK characters with their punctuation, plus words of
// other scripts without punctuation (or every non-space character). Markdown markup is left out
// unless 「统计 Markdown 标记」 is on. Speeds and both choices are set in 设置 › 外观.
const STATS_KEY = 'memex.m.stats';
const STATS_DEFAULT = { markup: false, method: 'mixed', zh: 400, en: 230 };
const statsPrefs = () => { try { return { ...STATS_DEFAULT, ...JSON.parse(localStorage.getItem(STATS_KEY) || '{}') }; } catch { return { ...STATS_DEFAULT }; } };
function setStatsPref(k, v) { localStorage.setItem(STATS_KEY, JSON.stringify({ ...statsPrefs(), [k]: v })); render(); }
const CJK = /[\u2e80-\u2fdf\u3000-\u303f\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\ufe30-\ufe4f\uff00-\uffef\u2014\u201c\u201d\u2026]/g; // ‘’ stay with words (It’s)
const stripMarkup = (md) => md.replace(/^\s*(```|~~~).*$/gm, '').replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
  .replace(/<[^>\n]+>/g, ' ').replace(/^\s{0,3}(#{1,6}|>+|[-*+]|\d+[.)])\s+/gm, '').replace(/^\s*\|?[\s:|-]+\|[\s:|-]*$/gm, '').replace(/[*_~`|\\]/g, ' ');
function countText(text, p = statsPrefs()) {
  const t = p.markup ? text : stripMarkup(text);
  if (p.method === 'chars') { const n = (t.match(/\S/gu) || []).length; return { cjk: n, words: 0 }; }
  const cjk = (t.match(CJK) || []).length;
  const words = (t.replace(CJK, ' ').match(/[\p{L}\p{N}]+(?:['’.-][\p{L}\p{N}]+)*/gu) || []).length;
  return { cjk, words };
}
function statsLine(it, pct) {
  const p = statsPrefs();
  if (it.book) return it.book.doc ? bookStatsLine(it, pct, p) : [];
  const partial = it.total != null && it.nextByte < it.total;
  const c = countText(it.text, p);
  const scale = partial && it.nextByte ? it.total / it.nextByte : 1;
  const cjk = Math.round(c.cjk * scale); const words = Math.round(c.words * scale);
  const approx = partial ? tx('约') : '';
  const count = p.method === 'chars' ? tx('{0} 字符', cjk.toLocaleString())
    : [cjk ? tx('{0} 字', cjk.toLocaleString()) : null, words ? tx('{0} 词', words.toLocaleString()) : null].filter(Boolean).join(' + ') || tx('0 字');
  const mins = Math.max(1, Math.round(cjk / p.zh + words / p.en));
  const segs = Math.max(1, Math.ceil((it.total || 0) / SEGMENT));
  const loaded = it.nextByte >= it.total ? segs : Math.min(segs, Math.ceil(it.nextByte / SEGMENT));
  const cache = it.cache === 'checking' ? tx('缓存版本') : it.cache === 'offline' ? tx('离线缓存')
    : it.cachedNext == null ? tx('未缓存') : it.cachedNext >= it.nextByte ? tx('已缓存') : tx('部分已缓存');
  return [`${approx}${count}`, tx('约 {0} 分钟', mins), tx('读到 {0}%', pct), `${tx('已载入 {0}/{1} 段', loaded, segs)} · ${cache}`];
}
// A book counts the chapter on screen (the original in 对照), labelled 本章.
function bookStatsLine(it, pct, p) {
  const b = it.book;
  const [side, ch] = bookShown(b)[0] || [];
  const e = ch && b.store.get(`${side}:${ch.key}`);
  const nav = bookNav(b, state.lang); const idx = bookIndex(b, nav, state.lang);
  const where = tx('第 {0}/{1} 章', idx >= 0 ? idx + 1 : '–', nav.length);
  if (!ch) return [where]; // no counterpart on this side: nothing to count
  const c = countText(e?.done ? e.text : '', p);
  const count = p.method === 'chars' ? tx('{0} 字符', c.cjk.toLocaleString())
    : [c.cjk ? tx('{0} 字', c.cjk.toLocaleString()) : null, c.words ? tx('{0} 词', c.words.toLocaleString()) : null].filter(Boolean).join(' + ') || tx('0 字');
  return [tx('本章 {0}', count), tx('约 {0} 分钟', Math.max(1, Math.round(c.cjk / p.zh + c.words / p.en))), tx('读到 {0}%', pct), where];
}
let hlTerms = []; // rendered 重点 (AI 重点标注 and 手工标记), in document order: { el, text, frac, style, manual, ctx }
let chromeKey = null;
// About ten characters of the sentence before a mark (from the rendered text), led by 「…」 when cut.
function termContext(el) {
  const blk = el.closest('[data-b], [data-bt], p, li, h1, h2, h3, h4, blockquote');
  if (!blk) return '';
  const r = document.createRange(); r.setStart(blk, 0); r.setEndBefore(el);
  let pre = r.toString().replace(/\s+/g, ' ');
  const spk = blk.dataset?.spk; if (spk && pre.startsWith(spk)) pre = pre.slice(spk.length).replace(/^\s*[:：]\s*/, '');
  const chars = [...pre];
  return chars.length > 10 ? `…${chars.slice(-10).join('')}` : pre;
}
function updateReaderChrome() {
  const sc = document.getElementById('reader-scroll');
  const it = state.item;
  if (!sc || !it?.meta || !DRAFTS) return;
  const max = sc.scrollHeight - sc.clientHeight;
  const frac = max > 0 ? Math.min(1, Math.max(0, sc.scrollTop / max)) : 1;
  const pct = Math.round(frac * 100);
  const X = lrRail(sc, dial?.thumbFrac ?? frac); // a turned dial shows its card's place on the rail
  if (X) {
    const tp = Math.round((dial?.thumbFrac ?? frac) * 100);
    document.getElementById('scrub-pct').textContent = `${tp}%`;
    document.getElementById('scrub').setAttribute('aria-valuenow', String(tp));
  }
  const strip = document.getElementById('stats-strip');
  if (strip) strip.replaceChildren(...statsLine(it, pct).flatMap((s, i) => (i ? [h('span', { class: 'sep' }, '·'), h('span', {}, s)] : [h('span', {}, s)])));
  const plainDoc = lrOn(it);
  if (plainDoc) { lrBubble(it, sc); lrTrackPosition(it, sc); if (lr(it).list) paintKlist(it); }
  const marks = [...sc.querySelectorAll('mark.hl[data-hl], mark.um[data-um]')];
  const key = `${marks.length}:${sc.scrollHeight}:${state.lang}:${plainDoc ? lr(it).bms.map((b) => b.byte).join(',') : ''}`;
  const ticks = document.getElementById('scrub-ticks');
  if (key !== chromeKey) {
    chromeKey = key;
    const base = rectIn(sc).top - sc.scrollTop;
    const at = (el) => Math.min(1, Math.max(0, (rectIn(el).top - base) / Math.max(1, sc.scrollHeight)));
    const seen = new Set(); const terms = [];
    for (const el of marks) {
      if (el.dataset.hl != null && !seen.has(`a${el.dataset.hl}`)) { seen.add(`a${el.dataset.hl}`); terms.push({ el, text: el.textContent, style: ['bold', 'highlight', 'positive', 'negative'].find((c) => el.classList.contains(c)) || 'other', frac: at(el), manual: false }); }
      for (const id of (el.dataset.ums || '').split(' ').filter(Boolean)) {
        if (seen.has(`m${id}`)) continue;
        seen.add(`m${id}`); // one card / tick per manual mark (a cross-chapter mark counts once)
        const parts = [...sc.querySelectorAll(`mark.um[data-ums~="${CSS.escape(id)}"]`)].map((x) => x.textContent).join('');
        terms.push({ el, text: [...parts].length > 12 ? `${[...parts].slice(0, 12).join('')}…` : parts, style: 'um', frac: at(el), manual: true });
      }
    }
    hlTerms = terms.sort((a, b) => a.frac - b.frac);
    for (const t of hlTerms) t.ctx = termContext(t.el);
    if (ticks) {
      const bms = plainDoc && state.lang !== 'trans' ? lr(it).bms.filter((b) => b.byte != null) : [];
      const bmF = bms.map((b) => { const y = lrYOfByte(b.byte); return y == null ? null : Math.min(1, (y + 8) / Math.max(1, sc.scrollHeight)); }).filter((f) => f != null);
      ticks.replaceChildren(...hlTerms.map((t) => h('i', { class: t.manual ? 'tick um' : `tick ${t.style}`, 'data-f': String(t.frac) })),
        ...bmF.map((f) => h('i', { class: 'bmt', 'data-f': String(f) })));
      document.getElementById('scrub').classList.toggle('has-ticks', hlTerms.length > 0);
    }
    if (plainDoc) lrPaintBms(it, sc);
  }
  // The ticks ride the track: where the thumb pushes it out, they move out with it.
  if (X && ticks) {
    const hgt = document.getElementById('scrub').clientHeight;
    for (const el of ticks.children) { const y = Number(el.dataset.f) * hgt; el.style.top = `${y.toFixed(1)}px`; el.style.left = `${(X(y) - (el.classList.contains('bmt') ? 13 : el.classList.contains('um') ? 3.5 : 2.5)).toFixed(1)}px`; }
  }
}
addEventListener('resize', () => { chromeKey = null; requestAnimationFrame(updateReaderChrome); });
function scrubberEl() {
  const point = (e) => {
    if (!dial) openDial(ptY(e), e.pointerType !== 'mouse');
    else { dial.y = ptY(e); dial.index = null; paintDial(); }
  };
  // Touch is owned by the edge-gesture handlers below (tap the track → jump there; drag → the
  // thumb follows; pull the thumb left → the dial). A mouse hovering the rail previews the dial,
  // dragging moves the thumb, clicking the track jumps; a pen drags the thumb.
  // One continuous track pushed out where the grey arch (the thumb) sits; 重点 ticks ride it.
  // Double-tap (touch, below) or double-click the thumb: a bookmark at the paragraph on the middle line.
  return h('div', { class: 'scrub', id: 'scrub', role: 'slider', 'aria-label': tx('重点导航'), 'aria-valuemin': '0', 'aria-valuemax': '100',
    tabindex: '0', onpointerenter: (e) => { if (e.pointerType === 'mouse') point(e); },
    ondblclick: (e) => { if (lrOnThumb(ptY(e))) { closeDial(); lrAddBookmark(); } },
    onpointerdown: (e) => { if (e.pointerType === 'touch') return; e.currentTarget.setPointerCapture(e.pointerId); if (!dial?.sticky) closeDial(); },
    onpointermove: (e) => { if (e.pointerType === 'touch') return; if (e.buttons) lrScrubTo(ptY(e)); else if (e.pointerType === 'mouse') point(e); },
    onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openDial(rectIn(e.currentTarget).top + e.currentTarget.clientHeight / 2, true); dial.el.focus(); } },
    onclick: (e) => { if (performance.now() - lrThumbTouch > 600 && !lrOnThumb(ptY(e))) { closeDial(); lrScrubTo(ptY(e)); } } },
  svgEl('svg', { class: 'scrub-svg', id: 'scrub-svg', 'aria-hidden': 'true' }), h('div', { class: 'scrub-ticks', id: 'scrub-ticks' }),
  h('div', { class: 'scrub-thumb', id: 'scrub-thumb' }, h('span', { id: 'scrub-pct' })));
}
// The dial: a frosted sheet over the article listing the marks around the finger. The nearest
// term is level and largest at the finger's height; farther ones fan out along an arc, turned
// and receding. Moving the finger turns it; releasing on a term (or tapping one) jumps there.
const svgEl = (tag, attrs) => { const el = document.createElementNS('http://www.w3.org/2000/svg', tag); for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v); return el; };
const DIAL_TILT_DEGREES = 2; // Per item away from center; visible endpoints stay within ±15°.
let dial = null; // { el, y, pull, sticky, center }
const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
function openDial(y, sticky) {
  const body = document.querySelector('.reader-body');
  if (!body) return;
  closeDial();
  if (state.item?.lr?.hlm) { state.item.lr.hlm = false; document.querySelector('.lr-hlm')?.remove(); } // the dial takes the screen
  updateReaderChrome(); // ticks are kept (never rebuilt under the finger)
  const el = h('div', { class: `dial${sticky ? ' sticky' : ''}`, role: 'listbox', tabindex: '0', 'aria-label': tx('重点'),
    onpointerleave: (e) => { if (e.pointerType === 'mouse' && !dial?.sticky && !dial?.drag && !e.relatedTarget?.closest?.('.scrub')) closeDial(); },
    onclick: (e) => { if (performance.now() < (dial?.suppressClickUntil || 0)) { e.stopPropagation(); return; } if (!e.target.closest('.dial-item')) closeDial(); },
    onkeydown: (e) => {
      const shifts = { ArrowDown: 1, ArrowUp: -1, PageDown: 8, PageUp: -8, Home: -hlTerms.length, End: hlTerms.length };
      if (e.key in shifts) { e.preventDefault(); turnDial(shifts[e.key]); }
      else if (e.key === 'Enter') { e.preventDefault(); jumpTerm(dial.center); }
      else if (e.key === 'Escape') closeDial();
    } });
  body.append(el);
  dial = { el, y, pull: 1, sticky, index: null };
  el.addEventListener('wheel', (e) => { e.preventDefault(); turnDial(e.deltaY * (e.deltaMode === 1 ? 16 : 1) / 44); }, { passive: false });
  el.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    dial.drag = { y: ptY(e), index: dial.index ?? dial.center, moved: false };
    // Capture on the persistent surface before the moving term nodes are replaced.
    el.setPointerCapture(e.pointerId);
  });
  el.addEventListener('pointermove', (e) => {
    if (!dial?.drag || !e.buttons) return;
    const delta = dial.drag.y - ptY(e);
    if (Math.abs(delta) < 5 && !dial.drag.moved) return;
    dial.drag.moved = true; el.setPointerCapture(e.pointerId);
    dial.index = Math.max(0, Math.min(hlTerms.length - 1, dial.drag.index + delta / 44));
    paintDial();
  });
  const end = (e) => {
    if (!dial) return;
    const tap = e.type === 'pointerup' && dial.drag && !dial.drag.moved;
    dial.suppressClickUntil = performance.now() + 400;
    dial.drag = null;
    if (tap) {
      const item = document.elementFromPoint(e.clientX, e.clientY)?.closest('.dial-item');
      if (item) jumpTerm(Number(item.dataset.k)); else shutDial(); // a tap beside the cards only puts the dial away
    }
  };
  el.addEventListener('pointerup', end); el.addEventListener('pointercancel', end);
  paintDial();
}
function turnDial(delta) {
  if (!dial || !hlTerms.length) return;
  dial.index = Math.max(0, Math.min(hlTerms.length - 1, (dial.index ?? dial.center) + delta));
  paintDial();
}
function closeDial() {
  const pending = dial?.needsRender;
  const moved = dial?.thumbFrac != null;
  dial?.el.remove(); dial = null;
  if (moved) updateReaderChrome(); // the thumb returns to the reading position
  // Let a chosen term's smooth jump settle before restoring the article DOM.
  if (pending) setTimeout(softRender, 600);
}
// An open dial (pulled out, or opened from the keyboard) is put away by a tap anywhere but a card;
// that tap does nothing else: no jump, no scroll, nothing underneath reacts (its press and click are
// swallowed; the touch gesture handlers below check dialShut too).
let dialShut = -Infinity; // when a tap last put the dial away
function shutDial() { closeDial(); dialShut = performance.now(); }
addEventListener('pointerdown', (e) => {
  if (dial?.sticky && !e.target.closest?.('.dial')) shutDial();
  if (performance.now() - dialShut < 400) { e.stopPropagation(); e.preventDefault(); }
}, true);
addEventListener('click', (e) => { if (performance.now() - dialShut < 700) { dialShut = -Infinity; e.stopPropagation(); e.preventDefault(); } }, true);
function paintDial() {
  const { el } = dial;
  el.style.setProperty('--pull', String(dial.pull));
  const box = rectIn(el);
  const track = rectIn(document.getElementById('scrub')) || box;
  const f = Math.min(1, Math.max(0, (dial.y - track.top) / Math.max(1, track.height)));
  const T = hlTerms;
  if (!T.length) {
    el.replaceChildren(h('div', { class: 'dial-empty' }, hlMode() === 'off' ? tx('重点都不显示（可在顶部「重点」里打开）') : state.emphasis ? tx('已载入的部分还没有重点标注') : tx('打开「重点标注」后，这里会列出全文的重点')));
    return;
  }
  // Fractional index of the finger among the marks (by position in the text).
  let c = 0;
  if (f >= T[T.length - 1].frac) c = T.length - 1;
  else if (f > T[0].frac) {
    const i = T.findIndex((t, k) => k + 1 < T.length && t.frac <= f && T[k + 1].frac >= f);
    c = i + (f - T[i].frac) / ((T[i + 1].frac - T[i].frac) || 1);
  }
  c = Math.max(0, Math.min(T.length - 1, dial.index ?? c));
  dial.center = Math.round(c);
  // Turning the dial moves the rail's thumb to the card's place (the article stays put until a card is chosen).
  if (dial.index != null) {
    const lo = Math.floor(c); const hi = Math.min(T.length - 1, lo + 1);
    const tf = T[lo].frac + (T[hi].frac - T[lo].frac) * (c - lo);
    if (tf !== dial.thumbFrac) { dial.thumbFrac = tf; updateReaderChrome(); }
  }
  // The level card starts at the finger's height but the cards always fill the sheet: near the top or
  // the bottom of the rail the arc slides so no band above or below is left empty.
  const margin = Math.min(74, box.height / 2);
  const top = 28; const bottom = box.height - 22;
  let pivot = Math.min(box.height - margin, Math.max(margin, dial.y - box.top));
  const fillTop = top + c * 44; const fillBottom = bottom - (T.length - 1 - c) * 44;
  if (fillBottom <= fillTop) pivot = Math.min(fillTop, Math.max(fillBottom, pivot));
  else pivot = Math.min(box.height - margin, Math.max(margin, Math.min(fillTop, pivot))); // too few cards to fill: keep them near the finger
  const first = Math.max(0, Math.ceil(c - (pivot - top) / 44));
  const last = Math.min(T.length - 1, Math.floor(c + (bottom - pivot) / 44));
  const kids = [h('div', { class: 'dial-count' }, tx('重点 {0} / {1}', dial.center + 1, T.length))];
  for (let k = first; k <= last; k += 1) {
    const d = k - c; const ad = Math.abs(d);
    const t = T[k];
    // Card: AI / 手工 tag, about ten characters of the sentence before it, the term itself, where.
    const item = h('button', { class: `dial-item ${t.manual ? 'um' : t.style}${k === dial.center ? ' near' : ''}`, role: 'option', 'aria-selected': String(k === dial.center), 'data-k': String(k),
      'aria-posinset': String(k + 1), 'aria-setsize': String(T.length),
      onclick: (e) => { e.stopPropagation(); if (performance.now() >= (dial?.suppressClickUntil || 0)) jumpTerm(k); } },
    t.manual ? h('span', { class: 'tg' }, tx('手工')) : h('span', { class: 'ta' }, 'AI'), t.ctx ? h('span', { class: 'dx' }, t.ctx) : null,
    h('span', { class: 'dt' }, t.text), h('span', { class: 'dp' }, `${Math.round(t.frac * 100)}%`));
    item.style.top = `${pivot}px`;
    item.style.transform = `translate(${(-Math.min(ad * ad * 2, box.width * 0.2)).toFixed(1)}px, calc(-50% + ${(d * 44).toFixed(1)}px)) rotate(${(-Math.max(-15, Math.min(15, d * DIAL_TILT_DEGREES))).toFixed(2)}deg) scale(${Math.max(0.8, 1 - ad * 0.035).toFixed(3)})`;
    item.style.opacity = String(Math.max(0.72, 1 - ad * 0.04));
    item.style.zIndex = String(20 - Math.round(ad));
    kids.push(item);
  }
  el.replaceChildren(...kids);
}
function jumpTerm(k) {
  const t = hlTerms[k];
  closeDial();
  if (!t?.el.isConnected) return;
  t.el.scrollIntoView({ block: 'center', behavior: reduced() ? 'auto' : 'smooth' });
  t.el.classList.add('flash'); setTimeout(() => t.el.classList.remove('flash'), 1800);
}

// ---------- 长文阅读 v2 (approved rev10): 目录浮窗, 重点四档, 鼓包进度条 + 书签, 旋钮, 选区标记, 说话人 ----------
// Ordinary documents only (a paired book keeps its chapter reader; ≡ 目录 opens its chapter list).
// Positions and marks are source-side: a block's first character (splitBlocks `start`) and the source
// map of its text nodes give exact UTF-8 byte offsets into the canonical body; the toc's `source`
// edition (sha256, section-* chapters) turns those into chapter-relative anchors (reader_v1 B2 and
// Manual reader annotations v1). AI translation text never supplies an anchor.
const TE = new TextEncoder();
const u8 = (s) => TE.encode(s).length;
const lrOn = (it) => !!it?.meta && !it.book;
const syncOn = () => readerOn() && state.caps?.reader_state_sync_v1 === 'available';
// Manual reader annotations v1 (frozen): a capability of its own, never the older reading_annotations_v1.
const ANNOTATIONS_CAP = 'reader_annotations_v1';
const annOn = () => readerOn() && state.caps?.[ANNOTATIONS_CAP] === 'available';
const lr = (it) => (it.lr ??= { toc: false, pin: false, tocTop: null, hlm: false, list: false, last: null, bms: [], ops: [], marks: [], stale: [], seq: 0, doc: null });
const srcEd = (it) => it.lr?.doc?.editions.find((e) => e.side === 'source') || null;
const hlMode = () => (state.emphasis ? (state.manualOn ? 'all' : 'ai') : (state.manualOn ? 'me' : 'off'));
function setHlMode(m) {
  state.emphasis = m === 'all' || m === 'ai'; state.manualOn = m === 'all' || m === 'me';
  if (state.emphasis) state.insView = 'text';
  render();
}
// Blocks of the loaded body with their UTF-8 start bytes, and the speakers named at paragraph starts.
// A bold label (**Name:** / **Name**:) is a speaker label too: podcast transcripts usually write it that way.
const SPEAKER = /^(?:\*\*|__)?([A-Za-z][A-Za-z .'-]{0,23}|[\p{Script=Han}·]{1,8})(?:\*\*|__)?\s*[:：]\s*(?:\*\*|__)?\s*[^\s*_]/u;
const NOT_SPEAKER = new Set(['注', '按', '译注', '原注', '问', '答', '原文', '译文', '摘要', '关键词', '关键字', '题记', '附', '附注', '说明', '提示', 'Note', 'Notes', 'Abstract', 'Keywords', 'Summary', 'Q', 'A']);
function lrBlocks(it) {
  const c = it.lrBlk;
  if (c && c.text === it.text) return c;
  const blocks = splitBlocks(it.text); const bytes = []; let pc = 0; let pb = 0; const byStart = new Map();
  for (const b of blocks) { pb += u8(it.text.slice(pc, b.start)); pc = b.start; bytes.push(pb); byStart.set(b.start, pb); }
  const counts = new Map();
  for (const b of blocks) { const m = !b.fenced && SPEAKER.exec(b.text); if (m && !NOT_SPEAKER.has(m[1].trim())) counts.set(m[1].trim(), (counts.get(m[1].trim()) || 0) + 1); }
  const spk = new Set([...counts].filter(([, n]) => n >= 2).map(([k]) => k));
  return (it.lrBlk = { text: it.text, blocks, bytes, byStart, spk });
}
// 说话人名字 (frozen contract "Speaker names and reference names"): a translated label is paired with an
// original one ONLY through the block correspondence 对照 already uses (pairBlocks: the item translation
// keeps the original's blocks one to one). A pair counts when both of its blocks start with an explicit
// speaker label; a translated name maps to an original name (and back) only when every such pair agrees.
// Never by the n-th labelled paragraph, paragraph counts or a substring. No correspondence: no mapping.
const spkName = (it, raw, side) => {
  const m = SPEAKER.exec(raw); const n = m?.[1].trim();
  return n && (side === 'o' ? lrBlocks(it).spk.has(n) : !NOT_SPEAKER.has(n)) ? n : null;
};
function spkPairs(it, ttext) {
  let c = it.lrSpkT;
  if (c && c.t === ttext && c.o === it.text) return c;
  const counts = new Map();
  for (const b of splitBlocks(ttext)) { const n = !b.fenced && spkName(it, b.text, 't'); if (n) counts.set(n, (counts.get(n) || 0) + 1); }
  const spk = new Set([...counts].filter(([, n]) => n >= 2).map(([k]) => k));
  // Only complete texts: a partly loaded side would pair the wrong blocks.
  const whole = it.nextByte >= it.total && it.translation?.next == null;
  const rows = whole ? pairBlocks(splitBlocks(it.text), splitBlocks(ttext)) : null;
  const map = new Map(); const rev = new Map(); const bad = new Set(); const badR = new Set();
  for (const r of rows || []) {
    if (r.shared || r.o.fenced || r.t.fenced) continue;
    const o = spkName(it, r.o.text, 'o'); const t = spkName(it, r.t.text, 't');
    if (!o || !t || !spk.has(t)) continue;
    if (map.has(t) && map.get(t) !== o) bad.add(t); else map.set(t, o);
    if (rev.has(o) && rev.get(o) !== t) badR.add(o); else rev.set(o, t);
  }
  for (const n of bad) map.delete(n);
  for (const n of badR) rev.delete(n);
  return (it.lrSpkT = { t: ttext, o: it.text, spk, map, rev, whole, rows: !!rows });
}
// The paragraph-start speaker label of a reader block as shown under 说话人名字: {show, s, e, ref} where
// [s, e) is the name in the raw block and ref=false when the name the setting asks for is unavailable
// (then the side's own label is shown). Only the display changes: bytes and source map are the text's.
function speakerLabel(it, side, raw) {
  const tt = it.translation?.text;
  let o = null; let t = null;
  const name = spkName(it, raw, side); if (!name) return null;
  if (side === 'o') o = name; else { if (!tt) return null; t = name; }
  const pick = state.spkName;
  const need = (side === 'o' && pick !== 'orig') || (side === 't' && pick !== 'trans');
  if (side === 't' && !spkPairs(it, tt).spk.has(name)) return null;
  if (need && tt) {
    const c = spkPairs(it, tt);
    // 只看译文 / 原文 read the rest of both texts (no model call) so the 对照 correspondence exists.
    if (!c.whole && !it.fullLoading && !it.fullFailed) loadFull();
    if (side === 'o') t = c.rev.get(name) || null; else o = c.map.get(name) || null;
  }
  const show = pick === 'trans' ? t || o : pick === 'both' && o && t ? (o === t ? o : `${o}（${t}）`) : o || t;
  const m = SPEAKER.exec(raw); const s = m.index + (/^(\*\*|__)/.test(raw) ? 2 : 0);
  return { show, s, e: s + name.length, ref: !need || (side === 'o' ? !!t : !!o) };
}
// Body byte → chapter anchor (at a boundary the following chapter, offset 0; EOF the last chapter's end).
function anchorAt(it, byte) {
  const ed = srcEd(it);
  if (!ed) return null;
  let ch = null;
  for (const c of ed.chapters) if (c.start <= byte) ch = c;
  if (!ch || byte > ch.end) return null;
  return { revision_id: it.meta.revision_id, side: 'source', sha256: ed.sha256, chapter: ch.key, offset: byte - ch.start };
}
// Anchor → body byte, only when it is bound to exactly the revision and edition shown (else null: stale).
function anchorByte(it, a) {
  const ed = srcEd(it);
  if (!ed || !a || a.revision_id !== it.meta.revision_id || a.side !== 'source' || a.sha256 !== ed.sha256) return null;
  const ch = ed.chapters.find((c) => c.key === a.chapter);
  return ch && a.offset <= ch.end - ch.start ? ch.start + a.offset : null;
}
function segmentsOf(it, s, e) {
  const ed = srcEd(it);
  if (!ed) return null;
  const out = [];
  for (const c of ed.chapters) { const a = Math.max(s, c.start); const b = Math.min(e, c.end); if (a < b) out.push({ chapter: c.key, start: a - c.start, end: b - c.start }); }
  return out.length && out.length <= 64 ? out : null;
}
function segmentBody(it, an) {
  const ed = srcEd(it);
  if (!ed || an.revision_id !== it.meta.revision_id || an.side !== 'source' || an.sha256 !== ed.sha256) return null;
  const out = [];
  for (const sg of an.segments || []) { const c = ed.chapters.find((x) => x.key === sg.chapter); if (!c || sg.end > c.end - c.start || sg.start >= sg.end) return null; out.push([c.start + sg.start, c.start + sg.end]); }
  return out.length ? out : null;
}
// 手工标记 pieces inside one rendered source block (raw character ranges), hidden by 只看 AI / 都不显示.
function manualFor(it, b, raw) {
  if (!state.manualOn || !it?.lr || !it.lr.marks.length) return [];
  const bs = lrBlocks(it).byStart.get(b.start);
  if (bs == null) return [];
  const be = bs + u8(raw); const out = [];
  for (const m of it.lr.marks) {
    for (const [s, e] of m.body || []) {
      if (e <= bs || s >= be) continue;
      const a = byteIdx(raw, Math.max(s, bs) - bs); const z = byteIdx(raw, Math.min(e, be) - bs);
      if (a >= 0 && z > a) out.push({ start: a, end: z, id: m.id, style: m.style, ord: m.ord, pend: m.status === 'pending' || m.status === 'unknown' });
    }
  }
  return out;
}
// AI marks (never overlapping) and manual pieces (may overlap) → disjoint pieces with every layer.
function mergeMarks(ai, um) {
  if (!um.length) return ai;
  const pts = [...new Set([...ai, ...um].flatMap((m) => [m.start, m.end]))].sort((a, b) => a - b);
  const out = [];
  for (let i = 0; i + 1 < pts.length; i += 1) {
    const a = pts[i]; const b = pts[i + 1];
    const x = ai.find((m) => m.start <= a && m.end >= b);
    const u = um.filter((m) => m.start <= a && m.end >= b).sort((p, q) => q.ord - p.ord || (p.id < q.id ? -1 : 1));
    if (!x && !u.length) continue;
    out.push({ start: a, end: b, n: x ? x.n : null, style: x?.style, label: x?.label, um: u.length ? u : null });
  }
  return out;
}
// bold / underline / yellow stack; red vs blue: the mark accepted last wins (stable id breaks a tie).
function umClasses(list) {
  const c = new Set();
  for (const m of list) if (['bold', 'underline', 'yellow'].includes(m.style)) c.add(`um-${m.style}`);
  const rb = list.find((m) => m.style === 'red' || m.style === 'blue');
  if (rb) c.add(`um-${rb.style}`);
  if (list.some((m) => m.pend)) c.add('um-pend');
  return [...c];
}

// ----- open / revision: toc, B2 state, the annotation ledger -----
function lrOpen(it, doc) {
  const L = lr(it);
  L.doc = doc; L.visit = freshRequestId(); // a deliberate open: a new visit (later saves keep it)
  lrLoad(it);
}
async function lrRevised(it) {
  // A newer revision replaced the body: old anchors are stale (kept, never moved onto the new text).
  const L = lr(it);
  L.doc = null; it.lrBlk = null; L.pos = null;
  try { L.doc = await source.readerToc({ item_id: it.id, revision_id: it.meta.revision_id }); } catch (e) { noteOutcome(e); }
  if (state.item !== it) return;
  await lrLoad(it, true);
}
async function lrLoad(it, revised = false) {
  const L = lr(it);
  try { await caps(); } catch { /* offline: local behaviour */ }
  if (state.item !== it) return;
  if (!L.doc && readerOn() && (syncOn() || annOn())) {
    try { L.doc = await source.readerToc({ item_id: it.id, revision_id: it.meta.revision_id }); } catch (e) { noteOutcome(e); }
  }
  if (!srcEd(it)) L.doc = null;
  await Promise.all([lrReadState(it, true, revised), lrReadMarks(it), psLoad(it)]);
  if (!syncOn()) lrLocalBms(it);
  if (state.item === it && route().name === 'item') softRender();
}
// Local-only bookmarks (no reader_state_sync_v1): kept on this phone, bound to the revision.
const BM_KEY = (id) => `memex.m.bm.${id}`;
function lrLocalBms(it) {
  const L = lr(it);
  let saved = null; try { saved = JSON.parse(localStorage.getItem(BM_KEY(it.id))); } catch { /* none */ }
  L.bms = (saved?.list || []).map((b) => ({ id: b.id, label: b.label, rev: b.rev, byte: b.rev === it.meta.revision_id ? b.byte : null, anchor: null }));
}
function lrStoreLocalBms(it) {
  localStorage.setItem(BM_KEY(it.id), JSON.stringify({ list: lr(it).bms.map((b) => ({ id: b.id, label: b.label, rev: b.rev, byte: b.byte })) }));
}
async function lrReadState(it, opening = false, revised = false) {
  const L = lr(it);
  if (!syncOn() || !srcEd(it)) { L.rs = null; return; }
  let r;
  try { r = await source.readerState({ item_id: it.id }); noteOutcome(null); } catch (e) { noteOutcome(e); L.rsErr = e.code || 'unknown'; return; }
  if (state.item !== it) return;
  L.rs = r; L.rsErr = null;
  L.bms = r.state.bookmarks.map((b) => ({ id: b.id, label: b.label, anchor: b.anchor, byte: anchorByte(it, b.anchor) }));
  if (!opening) return;
  const pos = r.state.position;
  if (!pos) return;
  const byte = anchorByte(it, pos);
  if (byte == null) {
    // Bound to another revision / edition: the article starts at the top; bookmarks stay as they are.
    it.scroll = 0; L.posStale = true;
    requestAnimationFrame(() => { const sc = document.getElementById('reader-scroll'); if (sc) sc.scrollTop = 0; toast(tx('文章已更新，从开头开始')); });
    return;
  }
  if (revised) return;
  L.restore = byte;
  await lrEnsureByte(it, byte);
  if (state.item !== it) return;
  requestAnimationFrame(() => requestAnimationFrame(() => { const y = lrYOfByte(byte); if (y != null && L.restore === byte) { const sc = document.getElementById('reader-scroll'); sc.scrollTop = y; it.scroll = y; } L.restore = null; }));
}
async function lrEnsureByte(it, byte) {
  for (let i = 0; i < 128 && state.item === it && lrBlocks(it).bytes.at(-1) < byte && it.nextByte < it.total; i += 1) {
    try { await loadSegment(); } catch (e) { noteOutcome(e); return; }
  }
  if (state.item === it) softRender();
}
// The complete next state: this visit, the position, every bookmark (never dropping another device's).
// A stale saved position (another revision) is cleared rather than re-sent.
const lrNext = (it) => { const L = lr(it); return { visit_id: L.visit, position: L.pos ?? (L.posStale ? null : L.rs?.state.position ?? null), bookmarks: L.bms.map((b) => ({ id: b.id, label: b.label, anchor: b.anchor })) }; };
let lrSaveTimer = null;
function lrSaveSoon(it, ms = 1500) { clearTimeout(lrSaveTimer); lrSaveTimer = setTimeout(() => lrSave(it), ms); }
async function lrSave(it) {
  const L = lr(it);
  if (!syncOn() || !L.rs) return;
  if (L.saving) { L.again = true; return; }
  L.saving = true;
  try {
    for (let round = 0; round < 2; round += 1) {
      // A lost reply is first replayed exactly (same state, same expected version): never a new visit.
      const req = L.pending || { item_id: it.id, expected_version: L.rs.version, state: lrNext(it) };
      if (!L.pending && JSON.stringify(req.state) === JSON.stringify(L.rs.state)) { L.ops = []; break; }
      try {
        const r = await source.readerSave(req); noteOutcome(null);
        const was = L.pending; L.pending = null; L.saveNote = null;
        if (r?.version && r.state) L.rs = r; else { await lrReadState(it); }
        if (!was) { L.ops = []; break; }
      } catch (e) {
        noteOutcome(e);
        if (e.code === 'stale_reader_state') {
          // Another device saved first: read it, re-apply only this phone's bookmark changes, save once more.
          L.pending = null; const ops = L.ops;
          await lrReadState(it);
          for (const op of ops) lrApplyOp(it, op);
          continue;
        }
        if (['unknown', 'unavailable', 'daemon_unavailable', 'library_loading', 'internal_error'].includes(e.code)) { L.pending = req; L.saveNote = 'unknown'; }
        else L.saveNote = e.code || 'unknown';
        break;
      }
    }
  } finally {
    L.saving = false;
    if (state.item === it && route().name === 'item') paintLrLine();
    if (L.again) { L.again = false; lrSaveSoon(it, 300); }
  }
}
function lrApplyOp(it, op) {
  const L = lr(it);
  if (op.kind === 'del') { L.bms = L.bms.filter((b) => b.id !== op.id); return; }
  const i = L.bms.findIndex((b) => b.id === op.bm.id);
  if (i >= 0) L.bms[i] = { ...op.bm }; else L.bms.push({ ...op.bm });
  L.bms.sort((a, b) => (a.byte ?? -1) - (b.byte ?? -1));
}
// The top visible source block → the position saved (orig / 对照, horizontal, text view only).
function lrTrackPosition(it, sc) {
  const L = lr(it);
  if (!syncOn() || !L.rs || L.restore != null || state.lang === 'trans' || state.insView !== 'text' || vertNow(it)) return;
  const top = rectIn(sc).top;
  const el = [...sc.querySelectorAll('[data-b]')].find((x) => rectIn(x).bottom > top + 4);
  if (!el) return;
  const b = lrBlocks(it).bytes[Number(el.dataset.b)];
  if (b == null) return;
  const a = anchorAt(it, b);
  if (!a || JSON.stringify(a) === JSON.stringify(L.pos)) return;
  L.pos = a; L.posStale = false;
  lrSaveSoon(it);
}

// ----- geometry helpers -----
const lrY = (sc, el) => rectIn(el).top - rectIn(sc).top + sc.scrollTop;
function lrYOfByte(byte) {
  const sc = document.getElementById('reader-scroll'); const it = state.item;
  if (!sc || !it) return null;
  const { bytes } = lrBlocks(it);
  let k = -1; for (let i = 0; i < bytes.length && bytes[i] <= byte; i += 1) k = i;
  const el = k >= 0 && sc.querySelector(`[data-b="${k}"]`);
  return el ? Math.max(0, lrY(sc, el) - 8) : null;
}
const lrPct = (sc, y) => { const max = sc.scrollHeight - sc.clientHeight; return max > 0 ? Math.round(Math.min(1, Math.max(0, y / max)) * 100) : 0; };
function lrGo(y, remember = true) {
  const sc = document.getElementById('reader-scroll');
  if (!sc) return;
  y = Math.min(sc.scrollHeight - sc.clientHeight, Math.max(0, y));
  if (remember && Math.abs(y - sc.scrollTop) > 40) lr(state.item).last = sc.scrollTop;
  sc.scrollTo({ top: y, behavior: reduced() ? 'auto' : 'smooth' });
}
// The four stops of ▲ / ▼: 纪要, 正文开头, 正文结尾, AI 对话记录 (null: not on screen yet).
function lrStops(sc) {
  const at = (sel, f) => { const el = sc.querySelector(sel); return el ? f(lrY(sc, el)) : null; };
  const max = sc.scrollHeight - sc.clientHeight;
  return [['sum', tx('纪要'), at('.minutes-card', (y) => Math.max(0, y - 10)) ?? 0], ['start', tx('正文开头'), at('[data-b], [data-bt]', (y) => Math.max(0, y - 10))],
    ['end', tx('正文结尾'), at('.lr-end', (y) => Math.max(0, Math.min(max, y - sc.clientHeight + 60)))], ['chat', tx('AI 对话记录'), at('.lr-chat', (y) => Math.min(max, Math.max(0, y - 10)))]];
}
async function lrStopsFull(it) {
  // 正文结尾 / AI 对话记录 sit after the whole body: load the rest first (bounded, read only).
  if (it.nextByte < it.total) { await loadFull(); await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))); }
  return document.getElementById('reader-scroll');
}
async function lrStep(d) {
  const it = state.item; let sc = document.getElementById('reader-scroll');
  if (!sc || !it) return;
  const L = lr(it); const y = sc.scrollTop;
  let st = lrStops(sc);
  const ex = st.findIndex(([, , v]) => v != null && Math.abs(v - y) < 24);
  let i;
  if (ex >= 0) i = ex + d;
  else { L.last = y; i = d < 0 ? st.map(([, , v]) => v != null && v < y).lastIndexOf(true) : st.findIndex(([, , v]) => v == null || v > y); }
  if (i < 0 || i > 3) i = ex >= 0 ? ex : d < 0 ? 0 : 3;
  if (st[i][2] == null) { sc = await lrStopsFull(it); if (!sc || state.item !== it) return; st = lrStops(sc); }
  const [, label, v] = st[i];
  if (v == null) { toast(tx('这一处还没有载入')); return; }
  sc.scrollTo({ top: v, behavior: reduced() ? 'auto' : 'smooth' });
  toast(label);
}

// ----- bookmarks (double-tap the thumb; drag a ribbon, it snaps to a paragraph start) -----
function lrMidBlock(sc) {
  const r = rectIn(sc); const mid = r.top + sc.clientHeight / 2;
  const els = [...sc.querySelectorAll('[data-b]')];
  return els.find((el) => { const b = rectIn(el); return b.top <= mid && b.bottom + 12 > mid; })
    || els.reduce((best, el) => (!best || Math.abs(rectIn(el).top - mid) < Math.abs(rectIn(best).top - mid) ? el : best), null);
}
function lrBmLabel(it, idx) { const t = lrBlocks(it).blocks[idx]?.text.replace(/^[#>*\-\s]+/, '').replace(/\s+/g, ' ') || ''; return t.length > 14 ? `${t.slice(0, 14)}…` : t; }
function lrPutBm(it, bm, kind) {
  const L = lr(it);
  lrApplyOp(it, { kind, bm });
  if (syncOn() && L.rs) { L.ops.push({ kind, bm: { ...bm } }); lrSave(it); } else lrStoreLocalBms(it);
  chromeKey = null; updateReaderChrome();
}
function lrAddBookmark() {
  const it = state.item; const sc = document.getElementById('reader-scroll');
  if (!lrOn(it) || !sc) return;
  if (state.lang === 'trans') { toast(tx('译文模式下不能加书签，请切到原文或对照')); return; }
  const el = lrMidBlock(sc);
  if (!el) return;
  const idx = Number(el.dataset.b); const byte = lrBlocks(it).bytes[idx];
  const L = lr(it);
  if (syncOn() && !L.rs) { toast(tx('阅读位置还没有从 Mac 读到，暂时不能加书签')); return; }
  if (L.bms.some((b) => b.byte === byte)) { toast(tx('这一段已有书签')); return; }
  const anchor = syncOn() && L.rs ? anchorAt(it, byte) : null;
  if (syncOn() && L.rs && !anchor) { toast(tx('这一段还不能加书签')); return; }
  lrPutBm(it, { id: `bm_${freshRequestId().slice(2, 18)}`, label: lrBmLabel(it, idx), anchor, byte, rev: it.meta.revision_id }, 'add');
  toast(tx('已加书签，可按住上下拖动'));
}
let lrTaps = 0; let lrThumbTouch = 0;
// The thumb's touch target: its 26 px arch plus 12 px above and below (app coordinates).
const lrOnThumb = (y) => { const th = rectIn(document.getElementById('scrub-thumb')); return !!th && y >= th.top - 12 && y <= th.bottom + 12; };
// Put the thumb's middle at y on the track: the article scrolls to the same fraction.
function lrScrubTo(y) {
  const sc = document.getElementById('reader-scroll'); const tr = rectIn(document.getElementById('scrub'));
  const max = sc ? sc.scrollHeight - sc.clientHeight : 0;
  if (!tr || max <= 0) return;
  sc.scrollTop = Math.min(1, Math.max(0, (y - tr.top - RAIL.TH / 2) / Math.max(1, tr.height - RAIL.TH))) * max;
}
// A tap on the track jumps the thumb there; on the thumb it only counts toward a double-tap (bookmark),
// so a jump followed by a quick tap on the thumb's new place never adds one.
function lrScrubTap(y) {
  const now = performance.now();
  lrThumbTouch = now; // the click this touch produces does nothing more
  if (!lrOnThumb(y)) { lrTaps = 0; lrScrubTo(y); return; }
  if (now - lrTaps < 340) { lrTaps = 0; lrAddBookmark(); } else lrTaps = now;
}
function lrPaintBms(it, sc) {
  const art = sc.querySelector('.article');
  if (!art) return;
  art.querySelectorAll('.bm').forEach((b) => b.remove());
  if (state.lang === 'trans' || state.insView !== 'text') return;
  const { bytes } = lrBlocks(it);
  for (const bm of lr(it).bms) {
    if (bm.byte == null) continue;
    const idx = bytes.indexOf(bm.byte); const blk = idx >= 0 && art.querySelector(`[data-b="${idx}"]`);
    if (!blk) continue;
    const top = rectIn(blk).top - rectIn(art).top + 2;
    const el = h('div', { class: 'bm', role: 'button', 'aria-label': tx('书签：{0}（按住上下拖动）', bm.label), 'data-bm': bm.id });
    el.style.top = `${top}px`;
    let d = null;
    el.addEventListener('pointerdown', (e) => { e.stopPropagation(); e.preventDefault(); el.setPointerCapture(e.pointerId); d = { y: ptY(e), t: top }; el.classList.add('drag'); });
    el.addEventListener('pointermove', (e) => { if (d) el.style.top = `${d.t + ptY(e) - d.y}px`; });
    const end = () => {
      if (!d) return;
      const y = parseFloat(el.style.top) + rectIn(art).top; d = null; el.classList.remove('drag');
      const best = [...art.querySelectorAll('[data-b]')].reduce((a, x) => (!a || Math.abs(rectIn(x).top - y) < Math.abs(rectIn(a).top - y) ? x : a), null);
      const k = Number(best?.dataset.b); const byte = bytes[k];
      if (byte == null || byte === bm.byte || lr(it).bms.some((b) => b.byte === byte)) { chromeKey = null; updateReaderChrome(); return; }
      const anchor = bm.anchor ? anchorAt(it, byte) : null;
      if (bm.anchor && !anchor) { chromeKey = null; updateReaderChrome(); return; }
      lrPutBm(it, { ...bm, anchor, byte, label: lrBmLabel(it, k), rev: it.meta.revision_id }, 'move');
      toast(tx('书签已移到这一段开头'));
    };
    el.addEventListener('pointerup', end); el.addEventListener('pointercancel', end);
    art.append(el);
  }
}

// ----- the bump rail (one continuous track pushed out where the thumb is; the grey arch fills it) -----
const RAIL = { TH: 26, D: 8, TR: 18 };
function lrRail(sc, frac) {
  const scrub = document.getElementById('scrub'); const svg = document.getElementById('scrub-svg');
  if (!scrub || !svg) return null;
  const W = scrub.clientWidth; const hgt = scrub.clientHeight; const BX = W - 9; const { TH, D, TR } = RAIL;
  const t0 = frac * (hgt - TH); const t1 = t0 + TH;
  const bump = (y) => { if (y >= t0 && y <= t1) return 1; const d = y < t0 ? t0 - y : y - t1; if (d >= TR) return 0; const u = 1 - d / TR; return u * u * (3 - 2 * u); };
  const X = (y) => BX - D * bump(y);
  const ys = []; for (let y = 0; y <= hgt; y += 2) ys.push(y);
  const l = ys.map((y) => `${(X(y) - 2.5).toFixed(1)},${y}`); const r = ys.slice().reverse().map((y) => `${(X(y) + 2.5).toFixed(1)},${y}`);
  const ty = []; for (let y = Math.max(0, Math.floor(t0 - TR)); y <= Math.min(hgt, t1 + TR); y += 1) ty.push(y);
  const al = ty.map((y) => `${(X(y) + 3.5).toFixed(1)},${y}`); const ar = ty.slice().reverse().map((y) => `${BX + 2.5},${y}`);
  svg.setAttribute('viewBox', `0 0 ${W} ${hgt}`);
  svg.innerHTML = `<path class="rail-track" d="M${l.join('L')}L${r.join('L')}Z"/><path class="rail-arch" d="M${al.join('L')}L${ar.join('L')}Z"/>`;
  const th = document.getElementById('scrub-thumb');
  th.style.top = `${t0}px`;
  return X;
}

// ----- the reader top bar, 目录 popover, 重点 menu, knobs, speaker bubble, notes line -----
function lrTopButtons(it) {
  const tocBtn = h('button', { class: `btn-sm lr-tb lr-tocb${lr(it).toc ? ' on' : ''}`, 'aria-label': tx('目录'), 'aria-expanded': String(!!lr(it).toc),
    onclick: (e) => {
      e.stopPropagation();
      if (it.book) { if (it.book.doc) { it.book.picker = true; render(); } return; }
      const L = lr(it); L.toc = !L.toc; if (L.toc) { L.list = false; L.hlm = false; } render();
    } }, '≡ ', tx('目录'));
  if (it.book) return [tocBtn];
  const names = { all: tx('全部'), ai: 'AI', me: tx('手工'), off: tx('关') };
  const m = hlMode();
  const hlBtn = h('button', { class: `btn-sm lr-tb lr-hlb${m !== 'off' ? ' on' : ''}`, 'aria-haspopup': 'menu', 'aria-expanded': String(!!lr(it).hlm), 'data-mode': m,
    onclick: (e) => { e.stopPropagation(); const L = lr(it); L.hlm = !L.hlm; if (L.hlm) { L.toc = L.pin ? L.toc : false; L.list = false; } render(); } }, tx('重点 · {0}', names[m]));
  return [tocBtn, hlBtn];
}
// Chapters for the 目录: the toc's source chapters (headings and their timestamps), or the headings
// of the body loaded so far. Never AI-made.
function lrSections(it) {
  const ed = srcEd(it);
  const split = (title) => { const m = /^(\d{1,2}:\d{2}(?::\d{2})?)\s+(.*)$/.exec(title.trim()); return m ? [m[1], m[2]] : [null, title.trim()]; };
  if (ed && ed.chapters.length > 1) return ed.chapters.map((c) => ({ byte: c.start, level: c.level, ...Object.fromEntries([['t', split(c.title)[0]], ['title', split(c.title)[1]]]) }));
  const { blocks, bytes } = lrBlocks(it); const out = [];
  blocks.forEach((b, i) => { const m = /^(#{1,6})[ \t]+(.*)$/.exec(b.text.split('\n')[0]); if (m && !b.fenced) { const [t, title] = split(m[2]); out.push({ byte: bytes[i], level: m[1].length, t, title }); } });
  return out;
}
function lrTocEl(it) {
  const L = lr(it);
  if (!L.toc) return null;
  const secs = lrSections(it);
  const sc = document.getElementById('reader-scroll');
  let cur = -1;
  if (sc) {
    const top = rectIn(sc).top + sc.clientHeight / 3;
    const el = [...sc.querySelectorAll('[data-b]')].filter((x) => rectIn(x).top <= top).pop();
    const b = el ? lrBlocks(it).bytes[Number(el.dataset.b)] : 0;
    secs.forEach((s, i) => { if (s.byte <= b) cur = i; });
  }
  const pinBtn = h('button', { class: `pb${L.pin ? ' on' : ''}`, 'aria-pressed': String(L.pin), onclick: (e) => {
    e.stopPropagation(); L.pin = !L.pin; render(); toast(L.pin ? tx('已钉住：跳转后不关闭，按住标题可上下拖动') : tx('已取消钉住'));
  } }, L.pin ? tx('已钉住') : tx('钉住'));
  const head = h('div', { class: 'th' }, h('span', { class: 'grip', 'aria-hidden': 'true' }, '⋮⋮'), h('span', { class: 'ttl' }, tx('目录')), pinBtn);
  let dg = null;
  head.addEventListener('pointerdown', (e) => { if (!L.pin || e.target.closest('.pb')) return; e.preventDefault(); head.setPointerCapture(e.pointerId); dg = { y: ptY(e), t: head.parentElement.offsetTop }; });
  head.addEventListener('pointermove', (e) => {
    if (!dg) return;
    const box = head.parentElement; const appH = app.clientHeight; const min = lrTocMin();
    L.tocTop = Math.max(min, Math.min(appH - box.offsetHeight - 60, dg.t + ptY(e) - dg.y)); box.style.top = `${L.tocTop}px`;
  });
  const up = () => { dg = null; }; head.addEventListener('pointerup', up); head.addEventListener('pointercancel', up);
  const rows = secs.length ? secs.map((s, i) => h('button', { class: `tr-row lv${Math.min(3, s.level)}${i === cur ? ' cur' : ''}`, 'aria-current': i === cur ? 'true' : null, onclick: () => lrJumpByte(it, s.byte, !L.pin) },
    s.t ? h('span', { class: 't' }, s.t) : null, h('span', { class: 'tl' }, zhShow(s.title))))
    : [h('div', { class: 'toc-empty' }, tx('这篇正文没有标题'))];
  const box = h('div', { class: `lr-toc${L.pin ? ' pinned' : ''}`, role: 'dialog', 'aria-label': tx('目录') }, head, h('div', { class: 'toc-rows' }, rows),
    h('div', { class: 'toc-cap' }, tx('来自原文的标题和时间标注')));
  if (L.tocTop != null && L.pin) box.style.top = `${L.tocTop}px`;
  return box;
}
const lrTocMin = () => { const t = document.querySelector('.reader-top'); return t ? rectIn(t).bottom - rectIn(app).top + 4 : 56; };
async function lrJumpByte(it, byte, close) {
  const L = lr(it);
  if (close) { L.toc = false; render(); }
  if (state.lang === 'trans') { state.lang = 'orig'; render(); }
  await lrEnsureByte(it, byte);
  if (state.item !== it) return;
  requestAnimationFrame(() => { const y = lrYOfByte(byte); if (y == null) { toast(tx('这一段还没有载入')); return; } lrGo(y); if (!close) setTimeout(() => softRender(), 500); });
}
function lrHlMenu(it) {
  const L = lr(it);
  if (!L.hlm) return null;
  const ai = insightsOn() ? (insightDone('highlights') || []).length : 0;
  const me = L.marks.length;
  const sw = (k) => h('span', { class: 'sw', 'aria-hidden': 'true' }, k.includes('a') ? h('i', { class: 'sw-ai' }) : null, k.includes('m') ? h('i', { class: 'sw-me' }) : null);
  const opt = (v, label, k, n) => h('button', { class: `o${hlMode() === v ? ' on' : ''}`, role: 'menuitemradio', 'aria-checked': String(hlMode() === v), 'data-v': v,
    onclick: (e) => { e.stopPropagation(); setHlMode(v); } }, sw(k), label, n == null ? null : h('span', { class: 'n' }, String(n)));
  let gen = null;
  if (insightsOn() && !insightDone('highlights')) {
    const [st, , why] = insightState(it, 'highlights');
    gen = st === 'run' ? h('div', { class: 'gen' }, h('span', { class: 'spin' }), tx('正在生成 AI 重点…'))
      : h('div', { class: 'gen' }, h('span', {}, st === 'bad' ? tx('AI 重点没有生成成功：{0}', why) : tx('这篇还没有 AI 重点。')),
        h('span', { class: 'cost' }, tx('使用摘要模型，长文可能分多次请求计费')),
        h('div', { class: 'row' }, h('button', { class: 'btn-sm', onclick: (e) => { e.stopPropagation(); state.emphasis = true; state.insView = 'text'; requestInsight('highlights'); } }, tx('生成 AI 重点'))));
  }
  return h('div', { class: 'lr-hlm', role: 'menu', 'aria-label': tx('重点') },
    opt('all', tx('都显示'), 'am', ai + me), opt('ai', tx('只看 AI'), 'a', ai), opt('me', tx('只看手工'), 'm', me), opt('off', tx('都不显示'), '', null), gen);
}
function lrKnobs(it) {
  const L = lr(it);
  const wake = (el) => { el.classList.add('act'); clearTimeout(lrKnobTimer); lrKnobTimer = setTimeout(() => { if (!lr(it).list) el.classList.remove('act'); }, 1800); };
  const up = h('button', { class: 'knob', 'aria-label': tx('上一个位置'), onclick: () => lrStep(-1) }, h('span', { class: 'cap' }, '▲'));
  const down = h('button', { class: 'knob', 'aria-label': tx('下一个位置'), onclick: () => lrStep(1) }, h('span', { class: 'cap' }, '▼'));
  const mid = h('button', { class: 'knob mid', 'aria-label': tx('按住选择跳转位置'), 'aria-expanded': String(!!L.list) }, h('span', { class: 'cap' }));
  const cap = mid.firstChild; let hd = null;
  mid.addEventListener('pointerdown', (e) => {
    e.preventDefault(); // a hold-and-slide never selects text
    mid.setPointerCapture(e.pointerId); mid.classList.add('press'); closeDial();
    hd = { y: ptY(e), long: false, was: L.list, t: setTimeout(() => { hd.long = true; L.list = true; L.hot = null; paintKlist(it); }, 220) };
  });
  mid.addEventListener('pointermove', (e) => {
    if (!hd) return;
    cap.style.transform = `translateY(${Math.max(-6, Math.min(6, (ptY(e) - hd.y) / 6))}px)`;
    if (!hd.long) return;
    const cols = [...document.querySelectorAll('.klist .col')]; const c1 = cols[cols.length - 1];
    if (!c1) return;
    const col = cols.length > 1 && ptX(e) < rectIn(c1).left ? cols[0] : c1;
    const its = [...col.querySelectorAll('.it:not([disabled])')];
    let best = null; let bd = 1e9;
    for (const b of its) { const r = rectIn(b); const d = Math.abs((r.top + r.bottom) / 2 - ptY(e)); if (d < bd) { bd = d; best = b; } }
    if (best && best.dataset.k !== L.hot) { L.hot = best.dataset.k; document.querySelectorAll('.klist .it').forEach((b) => b.classList.toggle('hot', b === best)); }
  });
  const end = (e) => {
    if (!hd) return;
    clearTimeout(hd.t); mid.classList.remove('press'); cap.style.transform = '';
    const h0 = hd; hd = null;
    if (e.type === 'pointercancel') return;
    if (h0.long) { if (L.hot) lrListJump(it, L.hot); else { L.list = false; paintKlist(it); } } else { L.list = !h0.was; L.hot = null; paintKlist(it); }
  };
  mid.addEventListener('pointerup', end); mid.addEventListener('pointercancel', end);
  mid.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); L.list = !L.list; paintKlist(it); } if (e.key === 'Escape') { L.list = false; paintKlist(it); } });
  const box = h('div', { class: `knobs${L.list ? ' act' : ''}`, role: 'group', 'aria-label': tx('位置旋钮') }, up, mid, down);
  box.addEventListener('pointerdown', () => wake(box));
  return [box, h('div', { class: 'klist', id: 'klist', hidden: !L.list })];
}
let lrKnobTimer = null;
function lrListItems(it, sc) {
  const L = lr(it);
  const st = Object.fromEntries(lrStops(sc).map(([k, , v]) => [k, v]));
  const c = chatFor(it.id); const turns = c.stored.filter((t) => t.role === 'user').length;
  const a = [['sum', '◆', tx('纪要'), st.sum, null], ['start', '▲', tx('正文开头'), st.start, null],
    ['last', '↺', tx('上次读到'), L.last, L.last != null ? `${lrPct(sc, L.last)}%` : '—'],
    ['end', '▼', tx('正文结尾'), st.end, null], ['chat', '✦', tx('AI 对话记录'), st.chat, turns ? String(turns) : null]];
  const bm = L.bms.map((b, k) => [`b${k}`, '', tx('书签 {0}', k + 1), b.byte == null ? null : lrYOfByte(b.byte), b.byte == null ? tx('旧版') : null, b]);
  return { a, bm };
}
function paintKlist(it) {
  const box = document.getElementById('klist'); const sc = document.getElementById('reader-scroll');
  const knobs = document.querySelector('.knobs');
  if (!box || !sc) return;
  const L = lr(it);
  box.hidden = !L.list; knobs?.classList.toggle('act', !!L.list); knobs?.querySelector('.mid')?.setAttribute('aria-expanded', String(!!L.list));
  if (!L.list) { box.replaceChildren(); return; }
  const { a, bm } = lrListItems(it, sc);
  L.items = Object.fromEntries([...a, ...bm].map((x) => [x[0], x]));
  const row = (x, k) => h('button', { class: `it${L.hot === x[0] ? ' hot' : ''}`, 'data-k': x[0], disabled: x[0] === 'last' && L.last == null ? true : null, onclick: () => lrListJump(it, x[0]) },
    k === 'b' ? h('span', { class: 'rb' }) : h('span', { class: 'g' }, x[1]), h('span', { class: 'lb' }, x[2]),
    x[4] ? h('span', { class: 'n' }, x[4]) : k === 'b' && x[3] != null ? h('span', { class: 'n' }, `${lrPct(sc, x[3])}%`) : null);
  const cols = [];
  if (bm.length) cols.push(h('div', { class: 'col c2', role: 'listbox', 'aria-label': tx('书签') }, h('div', { class: 'hd' }, tx('书签 · {0}', bm.length)), bm.map((x) => row(x, 'b'))));
  cols.push(h('div', { class: 'col c1', role: 'listbox', 'aria-label': tx('跳转位置') }, a.map((x) => row(x, 'a'))));
  box.replaceChildren(...cols);
}
async function lrListJump(it, k) {
  const L = lr(it); const x = L.items?.[k];
  L.list = false; L.hot = null; paintKlist(it);
  if (!x) return;
  let sc = document.getElementById('reader-scroll');
  if (k === 'last') { if (L.last == null) return; const was = sc.scrollTop; sc.scrollTo({ top: L.last, behavior: reduced() ? 'auto' : 'smooth' }); L.last = was; return; }
  if (x[5] && x[5].byte == null) { toast(tx('这个书签属于旧版本，正文已更新')); return; }
  if (x[3] == null && (k === 'end' || k === 'chat')) { sc = await lrStopsFull(it); if (!sc || state.item !== it) return; const v = Object.fromEntries(lrStops(sc).map(([kk, , vv]) => [kk, vv]))[k]; if (v != null) lrGo(v); return; }
  if (x[3] == null) { toast(tx('这一处还没有载入')); return; }
  lrGo(x[3]);
}
// The speaker whose label scrolled away: 「首字母 · 名字 · 正在说」 at the reader's top left.
function lrBubble(it, sc) {
  const bub = document.getElementById('lr-bubble');
  if (!bub) return;
  const L = lr(it);
  const top = rectIn(sc).top;
  const els = [...sc.querySelectorAll('[data-b], [data-bt]')];
  const k = els.findIndex((el) => rectIn(el).bottom > top + 4);
  let lab = null;
  for (let i = k; i >= 0; i -= 1) if (els[i].dataset.spk) { lab = els[i]; break; }
  const show = !!lab && rectIn(lab).top + 14 < top && !(L.toc && !L.pin) && state.insView === 'text';
  bub.hidden = !show;
  const key = `${lab?.dataset.spk}|${lab?.dataset.spkRef || ''}`;
  if (show && bub.dataset.name !== key) {
    const name = lab.dataset.spk; bub.dataset.name = key;
    // No reliable 对照 pairing for the name the setting asks for: its own label, said quietly.
    bub.replaceChildren(h('span', { class: 'av', 'aria-hidden': 'true' }, [...name][0].toUpperCase()), h('span', { class: 'nm' }, name),
      h('span', { class: 'cap' }, tx('正在说')), ...(lab.dataset.spkRef === 'none' ? [h('span', { class: 'cap ref-none', 'data-ref': 'none' }, tx('· 对照不可用'))] : []));
  }
}
// The line above the article: marks kept on this page only, an unknown mark / position save, stale marks.
function lrLine(it) {
  const L = lr(it);
  const bits = [];
  if (L.marks.some((m) => m.local)) bits.push(h('span', { class: 'lr-local' }, icon('info', 13), tx('本机暂存，未同步保存')));
  const unk = L.marks.find((m) => m.status === 'unknown');
  if (unk) bits.push(h('span', { class: 'warn' }, icon('warn', 13), tx('有一条标记不确定是否已保存'), ' ',
    h('button', { class: 'linkish', onclick: () => lrAnnRetry(it, unk) }, L.annPend?.checked ? tx('再试一次') : tx('核对'))));
  if (L.annErr) bits.push(h('span', { class: 'warn' }, icon('warn', 13), tx('标记没有读取：{0}', reasonText(L.annErr)), ' ', h('button', { class: 'linkish', onclick: () => lrReadMarks(it).then(() => softRender()) }, tx('重新读取'))));
  if (L.stale.length) bits.push(h('span', {}, tx('{0} 条旧版标记已保留（正文已更新，不显示）', L.stale.length)));
  if (L.saveNote === 'unknown') bits.push(h('span', { class: 'warn' }, tx('阅读位置可能没有保存')));
  return h('div', { class: 'lr-line', id: 'lr-line', hidden: !bits.length }, bits);
}
function paintLrLine() { const el = document.getElementById('lr-line'); if (el && state.item) el.replaceWith(lrLine(state.item)); }
// End of the article: 正文结尾, then the item's saved AI conversation (read only).
function lrEndEls(it) {
  const c = chatFor(it.id);
  const turns = c.stored;
  return [h('div', { class: 'lr-end' }, tx('正文结尾')), psCard(it),
    h('section', { class: 'lr-chat', 'aria-label': tx('AI 对话记录') }, h('div', { class: 'cap' }, turns.length ? tx('AI 对话记录 · {0}', turns.filter((t) => t.role === 'user').length) : tx('AI 对话记录')),
      turns.length ? turns.map((t) => h('div', { class: `lc ${t.role}` }, h('b', {}, t.role === 'user' ? tx('问') : tx('答')), t.role === 'user' ? t.content : mdDoc(t.content, { breaks: true })))
        : h('div', { class: 'lc-empty' }, c.hist === 'loading' ? tx('正在读取…') : c.hist === 'off' ? tx('这台 Mac 没有提供对话记录') : tx('这篇还没有 AI 对话记录')))];
}

// ----- 阅读记录 (reader_passes_v1, approved draft): 读完标记, 重读, 补记, opt-in 计时, 三种速率 -----
// The Mac keeps the ledger; this page never saves a pass locally. Writes are per-item CAS (version);
// a definite refusal is shown and nothing is kept; an unknown reply keeps the attempt, reads back and
// resends the IDENTICAL request only on a tap; stale_reader_passes reads again and replays the intent
// once. Timing only runs after the 计时 control is switched on (kept on this phone), only while the page
// is in front and a scroll / tap / key happened in the last 120 s, split at mode changes, hiding and the
// idle deadline, on a monotonic clock relative to the moment the session grant arrived.
const PS_IDLE = 120000; const PS_MAXSEG = 300000; const PS_GAP = 5000;
const PS_OPEN_KEY = (id) => `memex.m.passOpen.${id}`;
const PS_DEFINITE = new Set(['invalid_reader_pass', 'invalid_reader_pass_dates', 'invalid_reader_pass_timing', 'stale_reader_passes', 'stale_reader_pass_timing',
  'reader_pass_conflict', 'reader_pass_not_found', 'reader_pass_limit_exceeded', 'reader_pass_closed', 'reader_pass_not_timeable', 'reader_pass_session_expired',
  'reader_pass_clock_discontinuity', 'reader_pass_segment_conflict', 'library_resource_not_found', 'stale_item_revision', 'permission_denied', 'invalid_request',
  'unavailable', 'library_loading']);
const PS_METHODS = [['full', '全文通读'], ['sampled', '抽样阅读'], ['structural', '结构阅读']];
const psMethodName = (m) => (m ? tx(PS_METHODS.find((x) => x[0] === m)[1]) : tx('未选方式'));
const PS_ERR = { invalid_reader_pass: '填写的内容不被接受', invalid_reader_pass_dates: '结束早于开始', reader_pass_conflict: '这一遍的状态已经变了',
  reader_pass_not_found: '这一遍已不存在', reader_pass_limit_exceeded: '阅读记录已达上限', reader_pass_closed: '这一遍已结束，不再计时',
  stale_item_revision: '条目已在 Mac 上更新，请返回后重新打开', permission_denied: '这台手机没有修改权限', unavailable: '无法连接到你的 Mac', library_loading: 'Mac 正忙，稍后重试',
  stale_reader_passes: '阅读记录在别处改过了，请再试一次', stale_reader_pass_timing: '计时在别处更新了，请再试一次' };
const psErrText = (c) => (PS_ERR[c] ? tx(PS_ERR[c]) : reasonText(c));
let psClock = () => performance.now();
const psCap = () => state.caps?.reader_passes_v1 === 'available';
const psZone = () => { try { return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'; } catch { return 'UTC'; } };
const psDay = (tz, ms = Date.now()) => new Intl.DateTimeFormat('en-CA', { timeZone: tz, year: 'numeric', month: '2-digit', day: '2-digit' }).format(new Date(ms));
const psToday = () => ({ value: psDay(psZone()), precision: 'day' });
// Timing is on whenever the reader is in front (no switch to remember); ⏱ turns it off for this article only.
let psOffItem = null;
const psTimingPref = () => psOffItem == null || psOffItem !== state.item?.id;
const P$ = (it) => (it.ps ??= { st: 'idle', list: null, draft: null, edit: null, form: null, busy: false, pend: null, later: false, sheet: null, stats: {} });
const psPid = () => `pass_${freshRequestId().replace(/[^A-Za-z0-9]/g, '').slice(0, 24)}`;

// Date display (rendering only): the stored precision; this year (in the pass's zone) drops the year,
// a year-only date of this year reads 今年; unknown reads 未知.
const PS_MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
function psFmt(d, tz = psZone()) {
  if (!d) return tx('未知');
  const [y, m, dd] = d.value.split('-').map(Number); const cy = Number(psDay(tz).slice(0, 4));
  const en = uiLang === 'en';
  if (d.precision === 'year') return y === cy ? tx('今年') : en ? String(y) : tx('{0}年', y);
  if (d.precision === 'month') return en ? (y === cy ? PS_MON[m - 1] : `${PS_MON[m - 1]} ${y}`) : y === cy ? tx('{0}月', m) : tx('{0}年{1}月', y, m);
  return en ? (y === cy ? `${PS_MON[m - 1]} ${dd}` : `${PS_MON[m - 1]} ${dd}, ${y}`) : y === cy ? tx('{0}月{1}日', m, dd) : tx('{0}年{1}月{2}日', y, m, dd);
}
// Durations: integer milliseconds summed first, rounded only here.
function psHm(ms) {
  if (ms > 0 && ms < 60000) return tx('不到 1 分');
  const m = Math.round(ms / 60000);
  return m >= 60 ? (m % 60 ? tx('{0} 小时 {1} 分', Math.floor(m / 60), m % 60) : tx('{0} 小时', m / 60)) : tx('{0} 分', m);
}
const psEst = (p) => (p.estimated_duration_minutes == null ? tx('时长未填')
  : p.duration_display_unit === 'hours' && p.estimated_duration_minutes % 60 === 0 ? tx('约 {0} 小时', p.estimated_duration_minutes / 60)
    : `${tx('约')} ${psHm(p.estimated_duration_minutes * 60000)}`);
// 约时长 → whole minutes, exactly (hours × 60 must be an integer; nothing is rounded or clipped).
function psMinutes(raw, unit) {
  const s = String(raw ?? '').trim();
  if (!s) return { minutes: null };
  const m = /^(\d+)(?:\.(\d+))?$/.exec(s);
  if (!m) return { err: tx('时长请填数字') };
  let minutes;
  if (unit === 'minutes') { if (m[2] && /[1-9]/.test(m[2])) return { err: tx('分钟请填整数') }; minutes = BigInt(m[1]); } else {
    const k = (m[2] || '').length; const num = BigInt(m[1] + (m[2] || '')) * 60n; const den = 10n ** BigInt(k);
    if (num % den) return { err: tx('换算后不是整分钟，请改一下（不会自动取整）') };
    minutes = num / den;
  }
  if (minutes < 1n || minutes > 525600n) return { err: tx('时长要在 1 分钟到 525600 分钟（一年）之间') };
  return { minutes: Number(minutes) };
}
// Earliest / latest possible day of a PartialDate as YYYYMMDD (only to refuse an impossible order).
function psBound(d, late) {
  const [y, m, dd] = d.value.split('-').map(Number);
  const mm = m || (late ? 12 : 1);
  return y * 10000 + mm * 100 + (dd || (late ? new Date(Date.UTC(y, mm, 0)).getUTCDate() : 1));
}
const psCur = (it) => { const L = it.ps?.list; return L ? L.passes.filter((p) => p.kind === 'tracked' && p.status === 'in_progress' && p.revision_id === L.current_revision_id).sort((a, b) => b.ordinal - a.ordinal)[0] || null : null; };
const psLastDone = (it) => (it.ps?.list?.passes || []).filter((p) => p.kind === 'tracked' && p.status === 'completed').sort((a, b) => (b.completed_at_ms || 0) - (a.completed_at_ms || 0))[0] || null;
const psById = (it, id) => it.ps?.list?.passes.find((p) => p.pass_id === id) || null;
function psDraft(it) {
  const P = P$(it); const cur = psCur(it);
  if (P.draft && P.draft.for === (cur?.pass_id || null)) return P.draft;
  const open = localStorage.getItem(PS_OPEN_KEY(it.id));
  P.draft = { for: cur?.pass_id || null, method: cur?.method ?? null, start: cur ? cur.start_date : (open ? { value: open, precision: 'day' } : psToday()), end: psToday() };
  return P.draft;
}

// ----- reads -----
async function psRead(it) {
  const P = P$(it);
  if (!psCap() || !lrOn(it)) { P.st = 'off'; return; }
  if (P.st !== 'ok') P.st = 'loading';
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const passes = []; let version = null; let offset = 0; let r;
    try {
      for (let page = 0; page < 41 && offset != null; page += 1) {
        r = await source.passesList({ item_id: it.id, offset, limit: 100, expected_version: version });
        passes.push(...r.passes); version = r.version; offset = r.next_offset;
      }
      noteOutcome(null);
    } catch (e) {
      noteOutcome(e);
      if (e.code === 'stale_reader_passes' && attempt === 0) continue;
      if (state.item !== it) return;
      if (P.st !== 'ok') { P.st = 'err'; P.err = e.code || 'unknown'; }
      return;
    }
    if (state.item !== it) return;
    P.list = { version, current_revision_id: r.current_revision_id, passes, total: r.total };
    P.st = 'ok'; P.err = null;
    return;
  }
}
async function psLoad(it) {
  const P = P$(it);
  P.later = false; P.form = null; P.edit = null; P.sheet = null;
  if (psCap() && lrOn(it) && !localStorage.getItem(PS_OPEN_KEY(it.id))) localStorage.setItem(PS_OPEN_KEY(it.id), psToday().value); // a proposal, not a measured fact
  await psRead(it);
  if (state.item === it) { psPaint(it); psTick(); }
}

// ----- writes -----
// One edit; `build(version)` gives the change for the version read (null: the intent no longer applies).
async function psSend(it, req, build, label) {
  const P = P$(it);
  for (let attempt = 0; ; attempt += 1) {
    try {
      const r = await source.passesEdit(req);
      P.pend = null; noteOutcome(null);
      await psRead(it);
      return r;
    } catch (e) {
      noteOutcome(e);
      if (e.code === 'stale_reader_passes' && attempt === 0 && build) {
        await psRead(it); // another device wrote first: read again, replay the same intent once
        const change = P.list && build(P.list);
        if (!change) { toast(tx('阅读记录在别处改过了，请再试一次')); return null; }
        req = { item_id: it.id, expected_version: P.list.version, change };
        continue;
      }
      if (PS_DEFINITE.has(e.code)) { P.pend = null; toast(tx('没有保存：{0}', psErrText(e.code))); await psRead(it); return null; }
      // Unknown: the Mac may have applied it. Keep the attempt, read back; resend only on a tap.
      P.pend = { req, label, checked: false };
      await psReconcile(it);
      return P.pend ? 'unknown' : psById(it, req.change.pass_id) || 'ok';
    }
  }
}
function psApplied(it, c) {
  const p = psById(it, c.pass_id);
  if (c.op === 'create_tracked' || c.op === 'create_retrospective') return !!p;
  if (c.op === 'delete') return !p;
  if (c.op === 'complete') return p?.status === 'completed';
  if (c.op === 'reopen') return p?.status === 'in_progress';
  return !!p && p.method === c.method && JSON.stringify(p.start_date) === JSON.stringify(c.start_date) && JSON.stringify(p.end_date) === JSON.stringify(c.end_date)
    && p.estimated_duration_minutes === c.estimated_duration_minutes && p.duration_display_unit === c.duration_display_unit;
}
async function psReconcile(it) {
  const P = P$(it); const p = P.pend;
  if (!p) return;
  await psRead(it);
  p.checked = true;
  if (P.list && psApplied(it, p.req.change)) { P.pend = null; toast(tx('已核对：这一步已经保存')); }
}
async function psRetry(it) {
  const P = P$(it); const p = P.pend;
  if (!p || P.busy) return;
  P.busy = true; psPaint(it);
  if (!p.checked) await psReconcile(it);
  else await psSend(it, p.req, null, p.label); // the identical request: the Mac answers its first result if it did apply
  if (!P.pend && p.req.change.op === 'create_retrospective' && psById(it, p.req.change.pass_id)) { P.form = null; toast(tx('已补记一遍')); }
  P.busy = false; psPaint(it);
}
const psEditReq = (it, change) => ({ item_id: it.id, expected_version: it.ps.list.version, change });
async function psCreateTracked(it, method, start) {
  const pass_id = psPid();
  const mk = () => ({ op: 'create_tracked', pass_id, revision_id: it.meta.revision_id, method, start_date: start, timezone: psZone() });
  const r = await psSend(it, psEditReq(it, mk()), mk, tx('新的一遍'));
  return r && r !== 'unknown' ? psById(it, pass_id) : null;
}
async function psMarkDone(it) {
  const P = P$(it); const d = psDraft(it);
  if (P.busy || P.pend) return;
  P.busy = true; psPaint(it);
  try {
    let cur = psCur(it);
    // Outstanding time first; whatever cannot be synchronised is named, never added later.
    await psFlush(true);
    if (!cur) { cur = await psCreateTracked(it, d.method, d.start); if (!cur) return; }
    else if (cur.method !== d.method || JSON.stringify(cur.start_date) !== JSON.stringify(d.start)) {
      const mk = () => ({ op: 'update', pass_id: cur.pass_id, method: d.method, start_date: d.start, end_date: null, estimated_duration_minutes: null, duration_display_unit: null });
      const r = await psSend(it, psEditReq(it, mk()), mk, tx('阅读方式和日期'));
      if (!r || r === 'unknown') return;
      cur = psById(it, cur.pass_id);
    }
    const pid = cur.pass_id;
    const mk = (L) => { const p = (L.passes || []).find((x) => x.pass_id === pid); return p && p.status === 'in_progress' ? { op: 'complete', pass_id: pid, end_date: d.end, expected_timing_version: p.timing.timing_version } : null; };
    let r = await psSend(it, psEditReq(it, mk(P.list)), mk, tx('标记读完'));
    if (r === null && psById(it, pid)?.status === 'in_progress' && P.list) {
      // stale_reader_pass_timing (a late report elsewhere): read again, replay the completion once.
      const c = mk(P.list); if (c) r = await psSend(it, psEditReq(it, c), mk, tx('标记读完'));
    }
    if (!r || r === 'unknown') return;
    const lost = psPendingMs(pid);
    psDropPass(pid);
    localStorage.removeItem(PS_OPEN_KEY(it.id));
    P.draft = null; P.edit = null; P.later = false;
    const p = psById(it, pid);
    toast(lost > 0 ? tx('已标记读完；有 {0} 没有同步到 Mac，未计入', psHm(lost)) : tx('第 {0} 遍已标记读完', p?.display_rank ?? ''));
  } finally { P.busy = false; psPaint(it); psPaintBar(); }
}
async function psAgain(it) {
  const P = P$(it);
  if (P.busy || P.pend) return;
  P.busy = true; psPaint(it);
  localStorage.setItem(PS_OPEN_KEY(it.id), psToday().value);
  const p = await psCreateTracked(it, null, psToday()); // 开始第 n+1 遍: an explicit start
  P.busy = false; P.draft = null;
  if (p) { T.lastAct = psClock(); psTick(); }
  psPaint(it); psPaintBar();
}
async function psSaveForm(it) {
  const P = P$(it); const f = P.form;
  if (!f || P.busy || P.pend) return;
  const date = (x) => {
    const v = x.p === 'year' ? String(x.y) : x.p === 'month' ? `${x.y}-${String(x.m).padStart(2, '0')}` : `${x.y}-${String(x.m).padStart(2, '0')}-${String(x.d).padStart(2, '0')}`;
    if (x.p === 'day' && x.d > new Date(Date.UTC(x.y, x.m, 0)).getUTCDate()) return null;
    return { value: v, precision: x.p };
  };
  const start = date(f.start); const end = date(f.end);
  const lo = (x) => x.y * 10000 + (x.p === 'year' ? 101 : x.m * 100 + (x.p === 'day' ? x.d : 1));
  const hi = (x) => x.y * 10000 + (x.p === 'year' ? 1231 : x.m * 100 + (x.p === 'day' ? x.d : new Date(Date.UTC(x.y, x.m, 0)).getUTCDate()));
  const dur = psMinutes(f.n, f.u);
  f.err = !start || !end ? tx('这一天不存在，请改一下') : hi(f.end) < lo(f.start) ? tx('约结束早于约开始') : dur.err || null;
  if (f.err) { psPaint(it); return; }
  P.busy = true; psPaint(it);
  const pass_id = psPid();
  const mk = () => ({ op: 'create_retrospective', pass_id, method: f.method, start_date: start, end_date: end, estimated_duration_minutes: dur.minutes,
    duration_display_unit: dur.minutes == null ? null : f.u, timezone: psZone(), reference_revision_id: it.meta.revision_id });
  const r = await psSend(it, psEditReq(it, mk()), mk, tx('补记一遍'));
  P.busy = false;
  if (r && r !== 'unknown') { P.form = null; toast(tx('已补记一遍')); }
  psPaint(it);
}

// ----- the card at 正文结尾 -----
function psCard(it) {
  if (!lrOn(it)) return null;
  const P = P$(it);
  if (!psCap()) return state.caps ? h('div', { class: 'ps-card ps-off', id: 'ps-card' }, tx('当前 Mac 版本不支持阅读记录')) : null;
  if (P.st === 'loading' || P.st === 'idle') return h('div', { class: 'ps-card ps-off', id: 'ps-card' }, tx('正在读取阅读记录…'));
  if (P.st === 'err') return h('div', { class: 'ps-card ps-off', id: 'ps-card' }, tx('阅读记录没有读取：{0}', psErrText(P.err)), ' ',
    h('button', { class: 'linkish', onclick: () => psLoad(it) }, tx('重新读取')));
  if (P.st !== 'ok') return null;
  const cur = psCur(it); const done = !cur && psLastDone(it);
  const btn = (label, on, cls = '', extra = {}) => h('button', { class: `ps-btn${cls}`, disabled: P.busy || (P.pend && !extra.pendOk) ? true : null, onclick: on, ...extra.attrs }, label);
  const formBtn = P.form ? null : btn(tx('补记一遍'), () => { psFormOpen(it); }, '', { attrs: { 'data-ps': 'retro' } });
  const pend = P.pend ? h('div', { class: 'ps-warn', role: 'status' }, icon('warn', 13), tx('不确定这一步是否已保存：{0}', P.pend.label), ' ',
    h('button', { class: 'linkish', 'data-ps': 'retry', disabled: P.busy ? true : null, onclick: () => psRetry(it) }, P.pend.checked ? tx('再试一次') : tx('核对'))) : null;
  let body;
  if (done) {
    body = [h('div', { class: 'ps-h' }, h('span', { class: 't' }, tx('第 {0} 遍已读完', done.display_rank)), h('span', { class: 's' }, psMethodName(done.method))),
      h('div', { class: 'ps-row' }, btn(tx('开始第 {0} 遍', (P.list.total || 0) + 1), () => psAgain(it), ' pri', { attrs: { 'data-ps': 'again' } }),
        btn(tx('看这一遍的统计'), () => psOpenSheet(it, done.pass_id), '', { attrs: { 'data-ps': 'stats' } }), formBtn)];
  } else if (P.later) {
    body = [h('div', { class: 'ps-h' }, h('span', { class: 't' }, tx('第 {0} 遍 · 读完了？', cur ? cur.display_rank : (P.list.total || 0) + 1)),
      h('button', { class: 'linkish s', onclick: () => { P.later = false; psPaint(it); } }, tx('展开')))];
  } else {
    const d = psDraft(it);
    const n = cur ? cur.display_rank : (P.list.total || 0) + 1;
    const dateBtn = (k) => h('b', { role: 'button', tabindex: '0', 'data-ps': `date-${k}`, onclick: () => { P.edit = P.edit === k ? null : k; psPaint(it); } }, psFmt(d[k]));
    const edit = P.edit ? (() => {
      const inp = h('input', { type: 'date', class: 'ps-date-in', value: d[P.edit]?.precision === 'day' ? d[P.edit].value : '', max: psDay(psZone()), 'aria-label': P.edit === 'start' ? tx('起') : tx('止') });
      return h('div', { class: 'ps-dates' }, inp, h('button', { class: 'ps-btn', 'data-ps': 'date-ok', onclick: () => {
        if (!/^\d{4}-\d{2}-\d{2}$/.test(inp.value)) { toast(tx('请选一个日期')); return; }
        d[P.edit] = { value: inp.value, precision: 'day' }; P.edit = null; psPaint(it);
      } }, tx('好')));
    })() : null;
    const order = d.start && d.end && psBound(d.end, true) < psBound(d.start, false);
    body = [h('div', { class: 'ps-h' }, h('span', { class: 't' }, tx('第 {0} 遍 · 读完了？', n)),
      h('span', { class: 's', 'data-ps': 'dwell' }, cur && cur.timing.unique_ms > 0 ? tx('活跃停留 {0}', psHm(cur.timing.unique_ms)) : tx('未计时'))),
    h('div', { class: 'ps-row', role: 'radiogroup', 'aria-label': tx('阅读方式') }, PS_METHODS.map(([k, label]) => h('button', { class: `ps-tag${d.method === k ? ' on' : ''}`, role: 'radio', 'aria-checked': String(d.method === k), 'data-m': k,
      onclick: () => { d.method = d.method === k ? null : k; psPaint(it); } }, tx(label)))),
    h('div', { class: 'ps-dates' }, tx('起'), ' ', dateBtn('start'), ' → ', tx('止'), ' ', dateBtn('end'), h('span', { class: 'hint' }, tx('点日期可改'))),
    d.method ? null : h('div', { class: 'ps-dates small' }, tx('阅读方式可不选，之后再补')),
    order ? h('div', { class: 'ps-warn' }, tx('止早于起，请改一下日期')) : null,
    edit,
    h('div', { class: 'ps-row' }, btn(P.busy ? tx('正在保存…') : tx('标记读完'), () => psMarkDone(it), ' pri', { attrs: { 'data-ps': 'done', disabled: P.busy || P.pend || order ? true : null } }),
      btn(tx('稍后'), () => { P.later = true; P.edit = null; psPaint(it); }), formBtn)];
  }
  const passes = P.list.passes.filter((p) => !(cur && p.pass_id === cur.pass_id && !done)).sort((a, b) => b.display_rank - a.display_rank);
  const hist = passes.length ? h('div', { class: 'ps-hist', 'aria-label': tx('按填写日期排序的阅读记录') }, passes.map((p) => {
    const retro = p.kind === 'retrospective';
    const dates = retro ? `${tx('约')} ${psFmt(p.start_date, p.timezone)} → ${psFmt(p.end_date, p.timezone)}`
      : `${psFmt(p.start_date, p.timezone)} → ${p.status === 'completed' ? psFmt(p.end_date, p.timezone) : tx('进行中')}`;
    const dur = retro ? psEst(p) : p.timing.unique_ms > 0 ? psHm(p.timing.unique_ms) : tx('未计时');
    return h('button', { class: 'ps-hi', 'data-pass': p.pass_id, 'data-rank': String(p.display_rank), onclick: () => psOpenSheet(it, p.pass_id) },
      h('span', { class: 'n' }, tx('第 {0} 遍', p.display_rank)), h('span', { class: 'k' }, psMethodName(p.method)), retro ? h('span', { class: 'k bk' }, tx('补记')) : null,
      p.source_status === 'changed' ? h('span', { class: 'k' }, tx('正文已更新')) : null,
      h('span', { class: 'd' }, `${dates} · ${dur}`));
  })) : null;
  return h('section', { class: 'ps-card', id: 'ps-card', 'aria-label': tx('阅读记录'), 'data-state': done ? 'done' : 'draft' }, body, pend, P.form ? psForm(it) : null, hist);
}
function psFormOpen(it) {
  const now = psDay(psZone()).split('-').map(Number);
  P$(it).form = { method: null, start: { p: 'month', y: now[0], m: now[1], d: 1 }, end: { p: 'month', y: now[0], m: now[1], d: 1 }, n: '', u: 'hours', err: null };
  psPaint(it);
}
function psForm(it) {
  const P = P$(it); const f = P.form; const cy = Number(psDay(psZone()).slice(0, 4));
  const pd = (key, label) => {
    const x = f[key];
    const sel = (k, opts) => { const s = h('select', { 'data-f': key, 'data-k': k, 'aria-label': `${label} ${k}`, onchange: (e) => { x[k] = Number(e.target.value); } },
      opts.map(([v, t]) => h('option', { value: String(v), selected: v === x[k] ? true : null }, t))); return s; };
    return h('div', { class: 'l' }, h('span', { class: 'lb' }, label),
      h('div', { class: 'pc', role: 'radiogroup', 'data-f': key }, [['year', tx('年')], ['month', tx('年月')], ['day', tx('年月日')]].map(([k, t]) => h('button', { class: x.p === k ? 'on' : '', role: 'radio', 'aria-checked': String(x.p === k), 'data-p': k,
        onclick: () => { x.p = k; psPaint(it); } }, t))),
      sel('y', Array.from({ length: 41 }, (_, i) => cy - 40 + i).reverse().map((y) => [y, uiLang === 'en' ? String(y) : tx('{0}年', y)])),
      x.p !== 'year' ? sel('m', Array.from({ length: 12 }, (_, i) => [i + 1, uiLang === 'en' ? PS_MON[i] : tx('{0}月', i + 1)])) : null,
      x.p === 'day' ? sel('d', Array.from({ length: 31 }, (_, i) => [i + 1, uiLang === 'en' ? String(i + 1) : tx('{0}日', i + 1)])) : null);
  };
  const num = h('input', { type: 'text', inputmode: 'decimal', id: 'ps-bfn', value: f.n, 'aria-label': tx('约时长'), oninput: (e) => { f.n = e.target.value; } });
  const unit = h('select', { id: 'ps-bfu', 'aria-label': tx('单位'), onchange: (e) => { f.u = e.target.value; } },
    h('option', { value: 'minutes', selected: f.u === 'minutes' ? true : null }, tx('分钟')), h('option', { value: 'hours', selected: f.u === 'hours' ? true : null }, tx('小时')));
  return h('div', { class: 'ps-bf', id: 'ps-bf' }, h('div', { class: 'ps-h' }, h('span', { class: 't' }, tx('补记一遍')), h('span', { class: 's' }, tx('大约即可'))),
    h('div', { class: 'ps-row' }, PS_METHODS.map(([k, label]) => h('button', { class: `ps-tag${f.method === k ? ' on' : ''}`, 'data-bm': k, onclick: () => { f.method = f.method === k ? null : k; psPaint(it); } }, tx(label)))),
    pd('start', tx('约开始')), pd('end', tx('约结束')),
    h('div', { class: 'l' }, h('span', { class: 'lb' }, tx('约时长')), num, unit),
    f.err ? h('div', { class: 'ps-warn', role: 'alert', id: 'ps-bf-err' }, f.err) : null,
    h('div', { class: 'ps-row' }, h('button', { class: 'ps-btn pri', 'data-ps': 'retro-save', disabled: P.busy || P.pend ? true : null, onclick: () => psSaveForm(it) }, tx('记下这一遍')),
      h('button', { class: 'ps-btn', 'data-ps': 'retro-cancel', onclick: () => { P.form = null; psPaint(it); } }, tx('取消'))));
}
function psPaint(it) {
  if (state.item !== it) return;
  const old = document.getElementById('ps-card');
  if (old) { const n = psCard(it); if (n) old.replaceWith(n); else old.remove(); }
  psPaintBar();
}

// ----- the bottom line: ⏱ 计时中 / 计时关 · 第 n 遍 · 时长 · 待同步 · 阅读统计 -----
function psBarInfo(it) {
  if (!it || !lrOn(it) || !psCap() || it.ps?.st !== 'ok') return null;
  const cur = psCur(it); const on = psTimingPref() && (!!cur || !psLastDone(it)); // on before 第 1 遍 exists: it starts with the first real reading
  return { on, n: cur ? cur.display_rank : (it.ps.list.total || 0) + 1, time: cur ? psHm(cur.timing.unique_ms) : null, pend: cur ? psPendingMs(cur.pass_id) : 0 };
}
function psBar(it) {
  if (!psBarInfo(it)) return null;
  const el = h('div', { class: 'ps-bar', id: 'ps-bar' },
    h('button', { class: 'ps-tgl', 'data-ps': 'timing', onclick: () => psToggle(it) }),
    h('span', { class: 'ps-n' }), h('span', { class: 'ps-time' }), h('span', { class: 'ps-pend' }),
    h('span', { class: 'sep' }, '·'),
    h('button', { class: 'ps-open', 'data-ps': 'sheet', onclick: () => psOpenSheet(it, (psCur(it) || psLastDone(it) || [...it.ps.list.passes].sort((a, b) => b.display_rank - a.display_rank)[0])?.pass_id || null) }, tx('阅读统计')));
  psFillBar(el, it);
  return el;
}
// Only what changed is touched: replacing the text under a finger between press and release loses the tap.
function psFillBar(el, it) {
  const x = psBarInfo(it);
  const [tgl, n, time, pend] = el.children;
  const put = (e, t) => { if (e.textContent !== t) e.textContent = t; };
  tgl.classList.toggle('on', x.on); tgl.setAttribute('aria-pressed', String(x.on)); tgl.title = x.on ? tx('点击关闭计时') : tx('点击开启计时');
  put(tgl, `⏱ ${x.on ? tx('计时中') : tx('计时关')}`);
  put(n, tx('第 {0} 遍', x.n));
  put(time, x.time ? `· ${x.time}` : ''); time.hidden = !x.time;
  put(pend, x.pend > 0 ? `· ${tx('{0}待同步', psHm(x.pend))}` : ''); pend.hidden = !(x.pend > 0);
}
function psPaintBar() {
  const el = document.getElementById('ps-bar'); const it = state.item;
  if (!el) return;
  if (!psBarInfo(it)) el.remove(); else psFillBar(el, it);
}
async function psToggle(it) {
  const P = P$(it);
  if (psTimingPref() && (psCur(it) || !psLastDone(it))) { psOffItem = it.id; psTick(); psFlush(); psPaintBar(); return; }
  psOffItem = null;
  if (!psCur(it)) {
    // No unfinished pass on this revision: switching timing on is the confirmed start of a new one.
    if (P.busy || P.pend) return;
    P.busy = true; psPaint(it);
    const d = psDraft(it);
    const p = await psCreateTracked(it, d.method, done0(it) ? psToday() : d.start);
    P.busy = false; P.draft = null;
    if (!p) { psOffItem = it.id; psPaint(it); return; }
  }
  T.lastAct = psClock(); psTick(); psPaint(it);
}
const done0 = (it) => !!psLastDone(it) && !psCur(it);

// ----- the timer -----
const T = { item: null, session: null, starting: false, startFail: -Infinity, seg: null, lastAct: -Infinity, lastTick: null, queue: [], n: 0, flushing: false, lastFlush: 0 };
function psMode(it) {
  const t = it.translation; const ready = t && !t.error && t.status === 'succeeded';
  return state.lang === 'orig' || !ready ? 'source' : state.lang === 'bi' ? 'bilingual' : 'translation';
}
function psPendingMs(pid) {
  const q = T.queue.filter((s) => s.pass === pid).reduce((a, s) => a + s.end_offset_ms - s.start_offset_ms, 0);
  return q + (T.seg && T.seg.pass === pid ? Math.max(0, psClock() - T.seg.start) : 0);
}
function psDropPass(pid) {
  T.queue = T.queue.filter((s) => s.pass !== pid);
  if (T.seg?.pass === pid) T.seg = null;
  if (T.session?.pass === pid) T.session = null;
}
function psCloseSeg(end) {
  const s = T.seg; T.seg = null;
  if (!s) return;
  const a = Math.max(0, Math.round(s.start - s.t0)); const b = Math.min(s.window, Math.floor(end - s.t0));
  if (b - a >= 1) T.queue.push({ item: s.item, pass: s.pass, sid: s.sid, segment_id: `seg_${Date.now().toString(36)}_${(T.n += 1)}`, mode: s.mode, start_offset_ms: a, end_offset_ms: b });
}
function psTick() {
  const now = psClock();
  const it = route().name === 'item' ? state.item : null;
  const cur = it && it.meta && lrOn(it) && psCap() && it.ps?.st === 'ok' && psTimingPref() ? psCur(it) : null;
  const visible = document.visibilityState === 'visible';
  const gap = T.lastTick != null && now - T.lastTick > PS_GAP; // throttled, suspended or asleep: unobservable
  let closed = false;
  if (T.seg) {
    const s = T.seg;
    let end = Math.min(gap ? T.lastTick : now, T.lastAct + PS_IDLE);
    if (end >= now) end = null;
    if (end == null && (!cur || cur.pass_id !== s.pass || !visible || psMode(it) !== s.mode || T.session?.id !== s.sid)) end = now;
    const cap = Math.min(s.start + PS_MAXSEG, s.t0 + s.window);
    if (end == null && now >= cap) end = cap;
    if (end != null) {
      end = Math.min(end, cap); psCloseSeg(end); closed = true;
      if (end >= s.t0 + s.window && T.session?.id === s.sid) T.session = null; // the window is used up: a new session
    }
  }
  if (gap || !visible) T.lastAct = -Infinity; // resuming needs a fresh scroll / tap / key
  if (T.session && (!cur || T.session.pass !== cur.pass_id || now >= T.session.t0 + T.session.window - 1000)) T.session = null;
  const active = cur && visible && now - T.lastAct < PS_IDLE;
  // First real reading on an article with no pass yet: that is the start of 第 1 遍 (opening alone never
  // creates one; a finished article starts its next pass only from 开始第 n+1 遍).
  if (!cur && it && visible && now - T.lastAct < PS_IDLE && it.meta && lrOn(it) && psCap() && it.ps?.st === 'ok' && psTimingPref() && !psLastDone(it) && !T.onPs) { // a tap on the record card / bar is not reading (and must not race its own buttons)
    const P = P$(it);
    if (!P.busy && !P.pend && !P.autoTried) {
      P.autoTried = true; P.busy = true;
      const d = psDraft(it);
      psCreateTracked(it, d.method, d.start).then((pp) => { P.busy = false; P.draft = null; if (pp) { T.lastAct = psClock(); psTick(); } psPaint(it); });
    }
  }
  if (active && !T.seg) {
    if (T.session) T.seg = { item: it.id, pass: cur.pass_id, sid: T.session.id, t0: T.session.t0, window: T.session.window, mode: psMode(it), start: now };
    else psSession(it, cur);
  }
  T.lastTick = now;
  if (T.queue.length && (closed || now - T.lastFlush > 30000)) psFlush();
  if (it) psPaintBar();
}
async function psSession(it, cur) {
  if (T.starting || psClock() - T.startFail < 15000) return;
  T.starting = true;
  try {
    const r = await source.passesTimingStart({ item_id: it.id, pass_id: cur.pass_id });
    const t0 = psClock(); // offsets count from receiving the grant (monotonic)
    if (r.session_id && r.pass_id === cur.pass_id) T.session = { id: r.session_id, pass: cur.pass_id, t0, window: Math.min(PS_MAXSEG, r.window_ms || PS_MAXSEG) };
  } catch (e) {
    T.startFail = psClock();
    if (['reader_pass_closed', 'reader_pass_not_found', 'stale_item_revision', 'reader_pass_not_timeable'].includes(e.code)) { await psRead(it); psPaint(it); }
  } finally { T.starting = false; }
  psTick();
}
async function psFlush(wait = false) {
  if (T.flushing) { if (wait) { for (let i = 0; i < 50 && T.flushing; i += 1) await new Promise((r) => setTimeout(r, 100)); } return; }
  T.flushing = true; T.lastFlush = psClock();
  try {
    if (wait && T.seg) psCloseSeg(Math.min(psClock(), T.lastAct + PS_IDLE));
    const groups = new Map();
    for (const s of T.queue) if (!s.dead) { const k = `${s.item} ${s.pass} ${s.sid}`; if (!groups.has(k)) groups.set(k, []); groups.get(k).push(s); }
    for (const list of groups.values()) {
      for (let i = 0; i < list.length; i += 64) {
        const part = list.slice(i, i + 64); const { item, pass, sid } = part[0];
        try {
          const r = await source.passesTimingReport({ item_id: item, pass_id: pass, session_id: sid,
            segments: part.map((s) => ({ segment_id: s.segment_id, mode: s.mode, start_offset_ms: s.start_offset_ms, end_offset_ms: s.end_offset_ms })) });
          const ok = new Set((r.outcomes || []).map((o) => o.segment_id));
          T.queue = T.queue.filter((s) => !(s.sid === sid && ok.has(s.segment_id)));
          const p = state.item?.id === item && psById(state.item, pass);
          if (p && r.totals) p.timing = { ...p.timing, ...r.totals };
        } catch (e) {
          if (['reader_pass_session_expired', 'reader_pass_clock_discontinuity'].includes(e.code)) {
            // Never moved into a new session: these stay 待同步 (not counted); new time needs a new session.
            for (const s of part) s.dead = true;
            if (T.session?.id === sid) { T.session = null; if (T.seg?.sid === sid) { psCloseSeg(psClock()); T.queue.filter((s) => s.sid === sid).forEach((s) => { s.dead = true; }); } }
          } else if (PS_DEFINITE.has(e.code) && e.code !== 'unavailable' && e.code !== 'library_loading') for (const s of part) s.dead = true;
          // unknown / unavailable: kept with the same ids and payloads; the next flush resends them (duplicate is harmless)
        }
      }
    }
  } finally { T.flushing = false; }
  psPaintBar();
  const dw = document.querySelector('#ps-card [data-ps="dwell"]'); const cur = state.item && psCur(state.item);
  const dt = cur && (cur.timing.unique_ms > 0 ? tx('活跃停留 {0}', psHm(cur.timing.unique_ms)) : tx('未计时'));
  if (dw && cur && dw.textContent !== dt) dw.textContent = dt;
}
for (const type of ['pointerdown', 'touchstart', 'touchmove', 'wheel', 'keydown']) {
  document.addEventListener(type, (e) => { if (route().name === 'item') { T.lastAct = psClock(); T.onPs = !!e.target?.closest?.('#ps-card, #ps-bar, .ps-sheet, .sheet'); psTick(); } }, { capture: true, passive: true });
}
document.addEventListener('visibilitychange', () => { psTick(); if (document.visibilityState !== 'visible') psFlush(); });
addEventListener('pagehide', () => { psTick(); psFlush(); });
setInterval(() => psTick(), 1000);

// ----- 阅读统计 sheet -----
async function psOpenSheet(it, pid) {
  const P = P$(it);
  P.sheet = pid || 'none'; render();
  if (!pid) return;
  P.stats[pid] = { st: 'loading' };
  await psRead(it); // fresh totals and rates (reports since the last read moved them)
  const days = []; let tv = null; let offset = 0;
  try {
    for (let page = 0; page < 10 && offset != null; page += 1) {
      const r = await source.passesStats({ item_id: it.id, pass_id: pid, offset, limit: 400, expected_timing_version: tv });
      days.push(...r.days); tv = r.timing_version; offset = r.next_offset;
    }
    P.stats[pid] = { st: 'ok', days };
  } catch (e) { P.stats[pid] = { st: 'err', err: e.code || 'unknown' }; }
  if (state.item === it && P.sheet === pid) render();
}
const psN = (v) => (v >= 10 ? Math.round(v).toLocaleString() : (Math.round(v * 10) / 10).toLocaleString());
function psSheet(it) {
  const P = it.ps;
  if (!P?.sheet || !lrOn(it)) return [];
  const close = () => { P.sheet = null; render(); };
  const p = psById(it, P.sheet);
  const head = h('div', { class: 'ps-sh' }, h('span', { class: 't' }, p ? tx('阅读统计 · 第 {0} 遍', p.display_rank) : tx('阅读统计')),
    h('button', { class: 'ps-btn', 'data-ps': 'sheet-close', onclick: close }, tx('关闭')));
  const lab = (t) => h('div', { class: 'lab' }, t);
  let body;
  if (!p) body = [h('p', { class: 'ps-cap' }, tx('还没有阅读记录。读到正文结尾时可以标记读完，或补记一遍。'))];
  else {
    const retro = p.kind === 'retrospective'; const tm = p.timing; const R = p.rates;
    const pend = retro ? 0 : psPendingMs(p.pass_id);
    const total = retro ? h('div', {}, lab(tx('补记的约数时长（不是计时）')), h('div', { class: 'big' }, psEst(p)))
      : h('div', {}, lab(tx('设备上报的活跃停留（Mac 和手机合并，重叠只算一次）')), h('div', { class: 'big', 'data-ps': 'total' }, psHm(tm.unique_ms)),
        pend > 0 ? h('div', { class: 'ps-cap', 'data-ps': 'pending' }, tx('另有 {0}在本机待同步，未计入', psHm(pend))) : null);
    const modes = [['source', tx('原文'), tm.source_ms], ['bilingual', tx('对照'), tm.bilingual_ms], ['translation', tx('译文'), tm.translation_ms]];
    const stack = !retro && tm.unique_ms > 0 ? [h('div', { class: 'ps-stack' }, modes.filter((m) => m[2] > 0).map(([k, , ms]) => { const i = h('i', { class: `m-${k}` }); i.style.flex = String(ms); return i; })),
      h('div', { class: 'ps-leg' }, modes.map(([k, label, ms]) => h('span', { 'data-mode': k }, h('i', { class: `m-${k}` }), `${label} ${psHm(ms)}`)))] : [];
    const len = (l) => (l ? [l.words ? tx('{0} 词', l.words.toLocaleString()) : null, l.characters ? tx('{0} 字', l.characters.toLocaleString()) : null].filter(Boolean).join(' + ') || '—' : '—');
    const rate = (u) => (u ? [u.words_per_minute != null ? tx('{0} 词/分', psN(u.words_per_minute)) : null, u.characters_per_minute != null ? tx('{0} 字/分', psN(u.characters_per_minute)) : null].filter(Boolean).join(' ＋ ') || '—' : '—');
    const flags = [retro ? tx('约，按补记参考版本全文长度估算') : null, (retro ? R.equivalent_full_text : R.estimate) ? tx('等效全文速率，不是实际读速') : null].filter(Boolean);
    const T0 = R.minutes != null ? (retro ? psEst(p) : psHm(Math.round(R.minutes * 60000))) : '—';
    const row = (k, label, l, u) => h('tr', { 'data-rate': k }, h('td', {}, label), h('td', { class: 'r' }, l), h('td', { class: 'r' }, rate(u)));
    const table = h('div', {}, lab([tx('同一时长 {0} 的三种速率', T0), ...flags.map((f) => ` · ${f}`)].join('')),
      h('table', { class: 'ps-rates' }, h('tbody', {}, row('source', tx('按原文'), len(p.lengths?.source), R.source), row('translation', tx('按译文'), len(p.lengths?.translation), R.translation),
        row('combined', tx('按对照'), p.lengths?.source && p.lengths?.translation ? tx('原文＋译文') : '—', R.combined))));
    const st = P.stats[p.pass_id];
    let chart;
    if (retro) chart = h('div', {}, lab(tx('每天活跃停留')), h('div', { class: 'ps-cap' }, tx('补记的遍只有约数时长，不进每日柱状图')));
    else if (!st || st.st === 'loading') chart = h('div', {}, lab(tx('每天活跃停留')), h('div', { class: 'ps-cap' }, tx('正在读取…')));
    else if (st.st === 'err') chart = h('div', {}, lab(tx('每天活跃停留')), h('div', { class: 'ps-cap' }, tx('每日数据没有读取：{0}', psErrText(st.err))));
    else {
      const max = Math.max(1, ...st.days.map((d) => d.ms));
      chart = h('div', {}, lab(tx('每天活跃停留')), st.days.length ? h('div', { class: 'ps-bars' }, st.days.slice(-14).map((d) => {
        const bar = h('span', {}); bar.style.height = `${Math.max(2, Math.round(d.ms / max * 50))}px`;
        return h('div', { 'data-day': d.date, 'data-ms': String(d.ms) }, bar, psFmt({ value: d.date, precision: 'day' }, p.timezone), h('br'), psHm(d.ms));
      })) : h('div', { class: 'ps-cap' }, tx('还没有计时数据')));
    }
    body = [total, ...stack, table, chart,
      h('div', { class: 'ps-cap' }, tx('计时需手动开启；只在页面前台且 2 分钟内有操作时累计，是设备上报的活跃停留，不等于专注时间。没有译文时，译文与对照速率显示「—」。补记的遍只有约数时长，不进每日柱状图。'))];
  }
  return [h('div', { class: 'scrim ps-scrim', onclick: close }), h('section', { class: 'sheet ps-sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': tx('阅读统计') }, head, h('div', { class: 'body' }, body))];
}

// ----- selection: B 红 蓝 U 黄 | AI -----
const MARK_KINDS = [['bold', 'B', '加粗'], ['red', '红', '标红'], ['blue', '蓝', '标蓝（负面）'], ['underline', 'U', '下划线'], ['yellow', '黄', '黄色高亮']];
const markName = (style) => tx(MARK_KINDS.find((k) => k[0] === style)[2]);
let lrRenderLater = false;
function lrSelRange() {
  const sel = getSelection();
  if (!sel?.rangeCount || sel.isCollapsed) return null;
  // A book's 对照 can show the AI translation in a pane that is not #reader-scroll.
  const r = sel.getRangeAt(0); const sc = state.item?.book ? document.querySelector('.reader-body') : document.getElementById('reader-scroll');
  return sc && sc.contains(r.commonAncestorContainer) && r.toString().trim() ? r : null;
}
// A DOM boundary → [source-mapped text node, offset] (the nearest mapped text inside `root`), or null.
const mapped = (n) => SRC.has(n) || SPKL.has(n);
function srcNode(node, off, atEnd, root) {
  if (node.nodeType !== 3) {
    const walker = (root) => document.createTreeWalker(root, NodeFilter.SHOW_TEXT, { acceptNode: (n) => (mapped(n) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP) });
    if (!atEnd) {
      const kid = node.childNodes[off];
      let n = kid ? (kid.nodeType === 3 && mapped(kid) ? kid : walker(kid).nextNode()) : null;
      if (!n) { const w = walker(root); w.currentNode = kid || node; n = w.nextNode(); }
      if (!n) return null; node = n; off = 0;
    } else {
      const kid = node.childNodes[off - 1];
      let n = null;
      if (kid) { if (kid.nodeType === 3 && mapped(kid)) n = kid; else { const w = walker(kid); let x; while ((x = w.nextNode())) n = x; } }
      if (!n) return null; node = n; off = n.length;
    }
  }
  return [node, off];
}
// [text node, offset] → character index in the block's raw text; -1 inside a renamed speaker label.
function srcPos(node, off) {
  const L = SPKL.get(node);
  if (L) return off === 0 ? L.s : off === node.length ? L.e : -1;
  const at = SRC.get(node);
  return at == null ? null : at + off;
}
// A DOM boundary → body character index through the source map, null, or -1 (inside a renamed label).
function lrPoint(node0, off0, atEnd) {
  const p = srcNode(node0, off0, atEnd, document.getElementById('reader-scroll'));
  if (!p) return null;
  const [node, off] = p;
  const at = srcPos(node, off);
  if (at == null || at < 0) return at;
  const blk = node.parentElement?.closest('[data-b], [data-bt]');
  if (!blk || blk.dataset.b == null) return null;
  const b = lrBlocks(state.item).blocks[Number(blk.dataset.b)];
  return b ? b.start + at : null;
}
function lrSelInfo(r) {
  const it = state.item;
  if (it.book) return aimSelInfo(r);
  const inTr = (n) => !!(n.nodeType === 3 ? n.parentElement : n)?.closest('[data-bt], .tr');
  if (inTr(r.startContainer) || inTr(r.endContainer)) return { side: 'trans', why: tx('译文暂不支持保存标记') };
  if (gj(it.id).zh !== 'orig' && zhItem(it)) return { side: 'orig', why: tx('转换只影响显示，不修改原文') };
  const a = lrPoint(r.startContainer, r.startOffset, false); const z = lrPoint(r.endContainer, r.endOffset, true);
  if (a === -1 || z === -1) return { side: 'orig', why: tx('改名显示的说话人只能整个选中，不能从名字中间标记') };
  if (a == null || z == null || z <= a) return { side: 'orig', why: tx('这段选区无法对应到原文，不能标记') };
  return { side: 'orig', s: u8(it.text.slice(0, a)), e: u8(it.text.slice(0, z)) };
}
function lrSelBar() {
  const old = document.getElementById('lr-selbar');
  const it = state.item; const body = document.querySelector('.reader-body');
  if (!(lrOn(it) || it?.book) || !body || route().name !== 'item') { old?.remove(); return; }
  const r = lrSelRange();
  // No selection: drop the selection bar (a tapped mark's own bar stays until a tap elsewhere).
  if (!r) { if (old && !old.classList.contains('edit')) old.remove(); if (lrRenderLater) { lrRenderLater = false; softRender(); } return; }
  old?.remove();
  const info = lrSelInfo(r);
  if (!info) return; // a book selection outside its AI translation: no bar
  const btns = MARK_KINDS.map(([style, glyph, name]) => h('button', { class: `mk mk-${style}`, 'data-m': style, 'aria-label': tx(name), title: info.why || tx(name), disabled: info.why ? true : null,
    onclick: () => lrMark(style, info) }, tx(glyph)));
  const bar = h('div', { class: 'lr-selbar', id: 'lr-selbar', role: 'toolbar', 'aria-label': tx('选中文字') }, ...btns, h('span', { class: 'sx' }),
    h('button', { class: 'ai', 'aria-label': tx('带着选中文字问 AI'), onclick: () => lrQuote(r, info) }, 'AI'),
    info.why ? h('div', { class: 'why' }, info.why) : null);
  bar.addEventListener('pointerdown', (e) => e.preventDefault()); bar.addEventListener('mousedown', (e) => e.preventDefault());
  body.append(bar);
  const rr = rectIn(r); const br = rectIn(body);
  let top;
  if (matchMedia('(pointer: coarse)').matches) {
    // iOS/Android draw their own edit menu (拷贝 / 查询 / 翻译) above the selection and a page cannot
    // turn it off, so on touch our bar goes below the selection, clear of the end handle. Only when
    // there is no room below does it go above, leaving room for the system menu.
    const sc = document.getElementById('reader-scroll'); const floor = (sc ? rectIn(sc).bottom : br.bottom) - 96;
    top = rr.bottom + 30 + bar.offsetHeight <= floor ? rr.bottom - br.top + 30 : rr.top - br.top - bar.offsetHeight - 64;
  } else {
    top = rr.top - br.top - bar.offsetHeight - 8; if (top < 4) top = rr.bottom - br.top + 8;
  }
  bar.style.top = `${Math.max(4, Math.min(br.height - bar.offsetHeight - 4, top))}px`; // a selection running off screen keeps its bar visible
  bar.style.left = `${Math.max(8, Math.min(br.width - bar.offsetWidth - 8, (rr.left + rr.right) / 2 - br.left - bar.offsetWidth / 2))}px`;
}
let lrSelTimer = null;
document.addEventListener('selectionchange', () => { clearTimeout(lrSelTimer); lrSelTimer = setTimeout(lrSelBar, 120); });
// The bar for a manual mark tapped in the text: change its style (same id and anchor) or delete it.
function lrMarkBar(it, id, el, L = lr(it)) {
  document.getElementById('lr-selbar')?.remove();
  const m = L.marks.find((x) => x.id === id); const body = document.querySelector('.reader-body');
  if (!m || !body) return;
  const bar = h('div', { class: 'lr-selbar edit', id: 'lr-selbar', role: 'toolbar', 'aria-label': tx('手工标记') },
    ...MARK_KINDS.map(([style, glyph, name]) => h('button', { class: `mk mk-${style}${m.style === style ? ' on' : ''}`, 'data-m': style, 'aria-label': tx(name), 'aria-pressed': String(m.style === style),
      disabled: m.status === 'pending' || m.status === 'unknown' ? true : null, onclick: () => { bar.remove(); if (m.style !== style) lrRestyle(it, m, style, L); } }, tx(glyph))),
    h('span', { class: 'sx' }), h('button', { class: 'del', disabled: m.status === 'pending' || m.status === 'unknown' ? true : null, onclick: () => { bar.remove(); lrDelete(it, m, L); } }, tx('删除')));
  body.append(bar);
  const rr = rectIn(el); const br = rectIn(body);
  let top = rr.top - br.top - bar.offsetHeight - 8; if (top < 4) top = rr.bottom - br.top + 8;
  bar.style.top = `${top}px`; bar.style.left = `${Math.max(8, Math.min(br.width - bar.offsetWidth - 8, (rr.left + rr.right) / 2 - br.left - bar.offsetWidth / 2))}px`;
  setTimeout(() => document.addEventListener('pointerdown', (e) => { if (!e.target.closest('#lr-selbar')) bar.remove(); }, { once: true }), 0);
}
function lrAfterMark(it) {
  // Marking while only AI marks (or none) are shown switches back to 都显示, so the new mark is visible.
  if (!state.manualOn) { state.emphasis = true; state.manualOn = true; }
  getSelection()?.removeAllRanges();
  document.getElementById('lr-selbar')?.remove();
  render();
}
async function lrMark(style, info) {
  const it = state.item; const L = lr(it);
  if (info.why || info.s == null) return;
  if (info.ai) { await aimMark(style, info); return; }
  const saved = annOn() && !!srcEd(it);
  const segments = saved ? segmentsOf(it, info.s, info.e) : null;
  if (saved && !segments) { toast(tx('这段选区无法对应到原文，不能标记')); return; }
  const anchor = saved ? { revision_id: it.meta.revision_id, side: 'source', sha256: srcEd(it).sha256, segments } : null;
  const body = saved ? segmentBody(it, anchor) : [[info.s, info.e]];
  // The same range with the same style again is refused (the Mac would reject it as a duplicate).
  if (L.marks.some((m) => m.style === style && JSON.stringify(m.body) === JSON.stringify(body))) { toast(tx('这段已有同样的标记')); getSelection()?.removeAllRanges(); return; }
  if (!saved) {
    L.marks.push({ id: `local_${L.seq += 1}`, style, body, local: true, status: 'current', ord: 1e6 + L.seq });
    lrAfterMark(it); toast(tx('已标记为{0}，算作手工重点 · 本机暂存，未同步保存', markName(style)));
    return;
  }
  const m = { id: `am_${freshRequestId().slice(2, 26)}`, style, anchor, body, status: 'pending', ord: 1e6 + (L.seq += 1) };
  L.marks.push(m);
  lrAfterMark(it);
  await lrEdit(it, m, { op: 'put', annotation: { id: m.id, style, anchor } }, () => tx('已标记为{0}，算作手工重点', markName(style)));
}
// `L`: the edition ledger (lr(it)) or the AI translation ledger (aim(it)); both use the same edit flow.
async function lrRestyle(it, m, style, L = lr(it)) {
  if (m.local) { m.style = style; m.ord = 1e6 + (L.seq += 1); render(); return; }
  const prev = m.style; m.style = style; m.status = 'pending'; m.ord = 1e6 + (L.seq += 1); render();
  await lrEdit(it, m, { op: 'put', annotation: { id: m.id, style, anchor: m.anchor } }, () => tx('已改为{0}', markName(style)), () => { m.style = prev; }, L);
}
async function lrDelete(it, m, L = lr(it)) {
  if (m.local) { L.marks = L.marks.filter((x) => x !== m); render(); return; }
  m.status = 'pending'; render();
  await lrEdit(it, m, { op: 'delete', id: m.id }, () => { L.marks = L.marks.filter((x) => x.id !== m.id); L.stale = L.stale.filter((x) => x.id !== m.id); return tx('标记已删除'); }, undefined, L);
}
// One ledger edit under CAS. Conflict: read the ledger and apply the user's change once on top of it
// (it may already be there). Lost reply: the mark stays as tried (`unknown`), read back to reconcile;
// the same request (same expected version) is only sent again on an explicit tap.
async function lrEdit(it, m, change, done, undo = () => {}, L = lr(it)) {
  // The AI ledger is named on every edit (a delete without it would target the edition ledger).
  const req = L.ai ? { item_id: it.id, expected_version: L.annVersion, annotation_space: 'ai_translation', change } : { item_id: it.id, expected_version: L.annVersion, change };
  L.annPend = { req, m, done, undo, checked: false };
  await lrSend(it, false, L);
}
// Refusals made before anything is written; every other error leaves the edit unknown.
const LR_DEFINITE = new Set(['invalid_reader_annotation', 'invalid_reader_anchor', 'reader_annotation_conflict', 'reader_annotation_limit_exceeded',
  'reader_annotation_not_found', 'reader_annotations_unavailable', 'invalid_request', 'library_resource_not_found',
  'item_not_found', 'stale_item_revision', 'permission_denied', 'stale_reader_translation']);
async function lrSend(it, again = false, L = lr(it)) {
  const p = L.annPend;
  const reread = L.ai ? aimRead : lrReadMarks;
  if (!p) return;
  let msg = null; // shown after the re-render (a render drops an earlier toast)
  try {
    const r = await source.readerAnnotationsEdit(p.req); noteOutcome(null);
    L.annVersion = r.version; L.annPend = null; p.m.status = 'current'; msg = p.done();
  } catch (e) {
    noteOutcome(e);
    if (e.code === 'stale_reader_annotations' && !again) {
      await reread(it, p);
      if (lrApplied(L, p)) { L.annPend = null; p.m.status = 'current'; msg = p.done(); }
      else { p.req = { ...p.req, expected_version: L.annVersion }; if (state.item === it) await lrSend(it, true, L); return; }
    } else if (!LR_DEFINITE.has(e.code)) {
      // internal_error (e.g. sync failed after the rename), outcome_unknown and
      // anything unrecognised may have been committed: unknown, reconciled later.
      p.m.status = 'unknown';
    } else {
      L.annPend = null; p.undo(); p.m.status = 'current';
      if (p.req.change.op === 'put' && !L.server?.has(p.m.id)) L.marks = L.marks.filter((x) => x !== p.m);
      msg = e.code === 'reader_annotation_conflict' ? tx('这段已有同样的标记') : tx('标记没有保存：{0}', reasonText(e.code));
      // The AI translation changed under the selection: read it again (the marks with it); select again.
      if (L.ai && e.code === 'stale_reader_translation') {
        for (const sg of p.req.change.annotation?.anchor.segments || []) it.book?.ai.delete(sg.chapter);
        L.tried = false; L.note = tx('译文已更新，请重新选择'); msg = L.note; // kept on the line: the re-read re-renders
      }
    }
  }
  if (state.item === it && route().name === 'item') softRender();
  if (msg && state.item === it) toast(msg);
}
const lrApplied = (L, p) => {
  const c = p.req.change; const s = L.server?.get(c.op === 'put' ? c.annotation.id : c.id);
  return c.op === 'delete' ? !s : !!s && s.style === c.annotation.style;
};
async function lrAnnRetry(it, m, L = lr(it)) {
  const p = L.annPend;
  if (!p || p.m !== m) return;
  if (!p.checked) {
    // Read back first: the lost edit may have been applied.
    await (L.ai ? aimRead : lrReadMarks)(it, p); p.checked = true;
    const msg = lrApplied(L, p) ? (L.annPend = null, p.m.status = 'current', p.done()) : null;
    softRender(); if (msg) toast(msg); return;
  }
  p.m.status = 'pending'; softRender();
  await lrSend(it, false, L); // the identical request: the Mac returns the first result if it did apply it
}
// The whole ledger, page by page on one version (a page of a newer version restarts once).
async function lrReadMarks(it, keep = null) {
  const L = lr(it);
  if (!annOn() || !srcEd(it)) { if (!annOn()) L.annVersion = null; return; }
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const all = []; let version = null; let offset = 0;
    try {
      for (let page = 0; page < 50 && offset != null; page += 1) {
        const r = await source.readerAnnotationsList({ item_id: it.id, offset, limit: 100, expected_version: version });
        all.push(...r.annotations); version = r.version; offset = r.next_offset;
      }
    } catch (e) {
      noteOutcome(e);
      if (e.code === 'stale_reader_annotations' && attempt === 0) continue;
      L.annErr = e.code || 'unknown'; return;
    }
    if (state.item !== it) return;
    L.annErr = null; L.annVersion = version; L.server = new Map(all.map((a) => [a.id, a]));
    const local = L.marks.filter((m) => m.local || (m === keep?.m && !L.server.has(m.id)));
    const cur = []; const stale = [];
    all.forEach((a, i) => {
      const body = a.status !== 'stale' ? segmentBody(it, a.anchor) : null;
      (body ? cur : stale).push({ id: a.id, style: a.style, anchor: a.anchor, body, status: 'current', ord: i });
    });
    // The edit being reconciled keeps the object the UI shows (and the style it asked for).
    const x = keep && cur.find((c) => c.id === keep.m.id);
    if (x) { x.status = keep.m.status; if (keep.req.change.op === 'put') x.style = keep.req.change.annotation.style; keep.m = x; }
    L.marks = [...cur, ...local]; L.stale = stale;
    return;
  }
}

// ----- 译文标记 (reader_annotations_ai_translation_v1): five-style marks on a book chapter's AI translation -----
// A ledger of its own (annotation_space "ai_translation"), never mixed with the edition marks. Anchor:
// {revision_id, side:"ai_translation", segments:[{chapter, translation_version, sha256, start, end}]},
// start/end a UTF-8 byte range of the chapter's COMPLETE translation Markdown (the chunks joined in
// order, so a later chunk's bytes already include chunk.start), found through the renderer's source
// map, never by searching for the words. Without the capability nothing is saved or kept locally.
const aimOn = () => readerOn() && state.caps?.reader_annotations_ai_translation_v1 === 'available';
const aim = (it) => (it.aim ??= { ai: true, marks: [], stale: [], annVersion: null, annPend: null, server: null, annErr: null, seq: 0, tried: false });
// The ledger is read once per open of the item (and again after a stale translation or 重新读取).
function aimEnsure(it) {
  const A = aim(it);
  if (A.tried === it.meta.revision_id) return;
  A.tried = it.meta.revision_id;
  caps().catch(() => {}).then(() => (aimOn() ? aimRead(it) : null)).then(() => { if (state.item === it && route().name === 'item') softRender(); });
}
// Rendered AI blocks → {chapter, a (the translation shown), byte (block start in the complete body), raw}.
const AIB = new WeakMap();
// The AI translation `a` of original chapter `o`, from byte `base` of its complete body: blocks drawn
// with the source map and the saved marks bound to exactly this chapter, version and hash.
function aiBlocks(it, o, a, md, base) {
  let pc = 0; let pb = base;
  return splitBlocks(md).flatMap((b) => {
    pb += u8(md.slice(pc, b.start)); pc = b.start;
    if (b.fenced) return [h('pre', { class: 'code' }, fenceBody(b.text))];
    const um = aimFor(it, o, a, pb, b.text);
    const els = mdBlock(b.text, { marks: um.length ? mergeMarks([], um) : [], side: 't', zh: curZh(), tcy: vertNow(state.item), srcMap: true });
    for (const el of els) { AIB.set(el, { chapter: o.key, a, byte: pb, raw: b.text }); el.setAttribute('data-aib', String(pb)); if (isNoteText(b.text)) el.classList.add('lnote'); }
    return els;
  });
}
// Mark pieces inside one AI block (raw character ranges). A stale mark is in A.stale and never drawn.
function aimFor(it, o, a, bs, raw) {
  const A = it.aim; const tr = a.translation;
  if (!A?.marks.length || !tr) return []; // a book has no 重点 menu: its 译文标记 are always shown
  const be = bs + u8(raw); const out = [];
  for (const m of A.marks) {
    if (m.anchor.revision_id !== it.meta.revision_id) continue;
    for (const sg of m.anchor.segments) {
      if (sg.chapter !== o.key || sg.translation_version !== tr.version || sg.sha256 !== tr.sha256 || sg.end <= bs || sg.start >= be) continue;
      const s = byteIdx(raw, Math.max(sg.start, bs) - bs); const z = byteIdx(raw, Math.min(sg.end, be) - bs);
      if (s >= 0 && z > s) out.push({ start: s, end: z, id: m.id, style: m.style, ord: m.ord, pend: m.status === 'pending' || m.status === 'unknown' });
    }
  }
  return out;
}
// A DOM boundary → {chapter, a, byte} in the AI translation shown, or null.
function aiPoint(node, off, atEnd) {
  const p = srcNode(node, off, atEnd, document.querySelector('.reader-body'));
  const at = p && srcPos(p[0], p[1]); const blk = p?.[0].parentElement?.closest('[data-aib]'); const b = blk && AIB.get(blk);
  return at == null || at < 0 || !b ? null : { chapter: b.chapter, a: b.a, byte: b.byte + u8(b.raw.slice(0, at)) };
}
// A selection in a book: only its AI translation can be marked (null: no bar for this selection).
function aimSelInfo(r) {
  const inAi = (n) => !!(n.nodeType === 3 ? n.parentElement : n)?.closest('[data-aib]');
  const a0 = inAi(r.startContainer); const z0 = inAi(r.endContainer);
  if (!a0 && !z0) return null;
  if (!aimOn()) return { side: 'trans', why: tx('译文暂不支持保存标记') };
  if (!a0 || !z0) return { side: 'trans', why: tx('选区超出了 AI 译文，不能标记') };
  if (curZh()) return { side: 'trans', why: tx('转换后的文字不能标记，请切回原样显示') };
  const a = aiPoint(r.startContainer, r.startOffset, false); const z = aiPoint(r.endContainer, r.endOffset, true);
  if (!a || !z || a.chapter !== z.chapter || a.a !== z.a || z.byte <= a.byte) return { side: 'trans', why: tx('这段选区无法对应到译文，不能标记') };
  return { side: 'trans', ai: true, chapter: a.chapter, a: a.a, s: a.byte, e: z.byte };
}
async function aimMark(style, info) {
  const it = state.item; const A = aim(it);
  A.note = null;
  if (A.annVersion == null) await aimRead(it);
  if (state.item !== it) return;
  if (A.annVersion == null) { toast(tx('标记没有保存：{0}', reasonText(A.annErr || 'unknown'))); return; }
  const tr = info.a.translation;
  const anchor = { revision_id: it.meta.revision_id, side: 'ai_translation', segments: [{ chapter: info.chapter, translation_version: tr.version, sha256: tr.sha256, start: info.s, end: info.e }] };
  // The same range with the same style again is refused (the Mac would reject it as a duplicate).
  const key = (an) => JSON.stringify([an.revision_id, an.segments.map((sg) => [sg.chapter, sg.translation_version, sg.sha256, sg.start, sg.end])]);
  if (A.marks.some((m) => m.style === style && key(m.anchor) === key(anchor))) { toast(tx('这段已有同样的标记')); getSelection()?.removeAllRanges(); return; }
  const m = { id: `am_${freshRequestId().slice(2, 26)}`, style, anchor, status: 'pending', ord: 1e6 + (A.seq += 1) };
  A.marks.push(m);
  lrAfterMark(it);
  await lrEdit(it, m, { op: 'put', annotation: { id: m.id, style, anchor } }, () => tx('已在译文上标记为{0}', markName(style)), undefined, A);
}
// The whole AI ledger, page by page on one version and view version (a drifted page restarts once).
async function aimRead(it, keep = null) {
  const A = aim(it);
  if (!aimOn()) { A.annVersion = null; return; }
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const all = []; let version = null; let view = null; let offset = 0;
    try {
      for (let page = 0; page < 50 && offset != null; page += 1) {
        const r = await source.readerAnnotationsList({ item_id: it.id, offset, limit: 100, expected_version: version, annotation_space: 'ai_translation', expected_view_version: view });
        all.push(...r.annotations); version = r.version; view = r.view_version; offset = r.next_offset;
      }
    } catch (e) {
      noteOutcome(e);
      if (e.code === 'stale_reader_annotations' && attempt === 0) continue;
      A.annErr = e.code || 'unknown'; return;
    }
    if (state.item !== it) return;
    A.annErr = null; A.annVersion = version; A.server = new Map(all.map((a) => [a.id, a]));
    const cur = []; const stale = [];
    all.forEach((a, i) => (a.status === 'current' ? cur : stale).push({ id: a.id, style: a.style, anchor: a.anchor, status: 'current', ord: Number.isSafeInteger(a.order) ? a.order : i }));
    // The edit being reconciled keeps the object the UI shows (and the style it asked for).
    const x = keep && [...cur, ...stale].find((c) => c.id === keep.m.id);
    if (x) { x.status = keep.m.status; if (keep.req.change.op === 'put') x.style = keep.req.change.annotation.style; keep.m = x; }
    const local = A.marks.filter((m) => m === keep?.m && !A.server.has(m.id));
    A.marks = [...cur, ...local]; A.stale = stale;
    return;
  }
}
// The line above an AI translation: an unknown edit (核对 / 再试一次), a failed read, and this chapter's
// stale marks (译文已更新，标记未迁移), which can only be deleted.
function aimLine(it, o) {
  if (!aimOn()) return null;
  const A = aim(it); const bits = [];
  if (A.note) bits.push(h('span', { class: 'warn', 'data-aim': 'note' }, icon('warn', 13), A.note));
  const unk = [...A.marks, ...A.stale].find((m) => m.status === 'unknown');
  if (unk) bits.push(h('span', { class: 'warn' }, icon('warn', 13), tx('有一条标记不确定是否已保存'), ' ',
    h('button', { class: 'linkish', 'data-aim': 'retry', onclick: () => lrAnnRetry(it, unk, A) }, A.annPend?.checked ? tx('再试一次') : tx('核对'))));
  if (A.annErr) bits.push(h('span', { class: 'warn' }, icon('warn', 13), tx('标记没有读取：{0}', reasonText(A.annErr)), ' ', h('button', { class: 'linkish', onclick: () => aimRead(it).then(() => softRender()) }, tx('重新读取'))));
  for (const m of A.stale.filter((x) => x.anchor.segments.some((sg) => sg.chapter === o.key))) {
    bits.push(h('span', { class: 'aim-stale', 'data-aim-stale': m.id }, h('i', { class: `aim-chip um-${m.style}` }, markName(m.style)), tx('译文已更新，标记未迁移'), ' ',
      h('button', { class: 'linkish', disabled: m.status !== 'current' ? true : null, onclick: () => lrDelete(it, m, A) }, tx('删除'))));
  }
  return h('div', { class: 'lr-line aim-line', hidden: !bits.length }, bits);
}

// ----- AI 引用: the selection prefilled as a quote in the existing conversation composer -----
function lrQuote(r, info) {
  const it = state.item;
  const text = r.toString().trim();
  if (u8(text) > 4000) { toast(tx('选中文字太长，请少选一些再引用')); return; }
  state.chatQuote = { item: it.id, text, side: info.side, revision: it.meta.revision_id };
  getSelection()?.removeAllRanges(); document.getElementById('lr-selbar')?.remove();
  state.drawerOpen = true; localStorage.setItem('memex.m.aiFolded', '0');
  render();
  requestAnimationFrame(() => document.querySelector('.drawer textarea')?.focus());
}
const revShort = (r) => String(r || '').slice(-8);
function quoteLabel(q) { return q.side === 'trans' ? tx('AI 译文 · 对应修订 {0}', revShort(q.revision)) : tx('原文 · 修订 {0}', revShort(q.revision)); }
function quoteEl(id) {
  const q = state.chatQuote;
  if (!q || q.item !== id) return null;
  const drift = state.item?.meta && state.item.meta.revision_id !== q.revision;
  return h('div', { class: 'chat-quote' }, h('div', { class: 'qh' }, h('b', {}, tx('引用')), h('span', { class: 'qs' }, quoteLabel(q)),
    h('button', { class: 'qx', 'aria-label': tx('移除引用'), onclick: () => { state.chatQuote = null; render(); } }, icon('x', 14))),
    h('div', { class: 'qt' }, q.text.length > 120 ? `${q.text.slice(0, 120)}…` : q.text),
    drift ? h('div', { class: 'qd warn' }, tx('条目已更新，这段引用来自之前的版本')) : null,
    h('div', { class: 'qn' }, tx('点发送才会提问。Mac 仍会带上这篇文章的正文和信息；回答不一定只依据引用，也不锁定版本。')));
}
// The quote becomes context in front of the question (the chat wire has no quote field).
function withQuote(id, question) {
  const q = state.chatQuote;
  if (!q || q.item !== id) return question;
  state.chatQuote = null;
  const drift = state.item?.meta && state.item.meta.revision_id !== q.revision;
  return `${tx('引用（{0}）', quoteLabel(q))}${drift ? tx('（来自之前的版本）') : ''}：\n${q.text.split('\n').map((l) => `> ${l}`).join('\n')}\n\n${question}`;
}
// The 目录 popover and the 重点 menu sit under the top bar (a pinned 目录 keeps where it was dragged).
function lrPlace() {
  const it = state.item; const top = lrTocMin();
  const t = document.querySelector('.lr-toc');
  if (t && !(lr(it).pin && lr(it).tocTop != null)) t.style.top = `${top}px`;
  const m = document.querySelector('.lr-hlm');
  if (m) m.style.top = `${top}px`;
}
// A tap elsewhere closes the 重点 menu, an unpinned 目录 and the knob list.
document.addEventListener('pointerdown', (e) => {
  const it = state.item;
  if (!it?.lr || route().name !== 'item') return;
  const L = it.lr;
  let changed = false;
  if (L.hlm && !e.target.closest?.('.lr-hlm, .reader-top')) { L.hlm = false; changed = true; }
  if (L.toc && !L.pin && !e.target.closest?.('.lr-toc, .reader-top')) { L.toc = false; changed = true; }
  if (L.list && !e.target.closest?.('.klist, .knobs')) { L.list = false; L.hot = null; paintKlist(it); }
  if (changed) render();
}, true);

// ---------- edge swipes (approved layout) ----------
// Left edge → library navigation. The right panel (item details) is opened only from its top-bar button:
// it is used rarely, so the right edge belongs to the scrubber (.scrub sits at the screen edge). The
// first 8 px of movement decide: mostly horizontal in the opening direction drags the panel with
// the finger, anything else is left to the page (vertical scrolling is never taken). A swipe back
// on an open panel closes it. On the scrubber a tap
// jumps the thumb, a drag moves it, and pulling the thumb left past DIAL_KEEP opens the dial (released
// short of it, the dial goes away). In a Safari tab (not the home-screen app) the outermost left strip
// belongs to Safari's own back swipe, so the left zone starts inset there; the ☰ button always works.
const EDGE = 24;
const DIAL_PULL = 140; // px of leftward pull for the whole dial
const DIAL_KEEP = 0.4; // pulled at least this far (56 px) when released: the dial stays open
const standaloneApp = navigator.standalone === true || matchMedia('(display-mode: standalone)').matches;
const LEFT_ZONE = standaloneApp ? [0, EDGE] : [12, 12 + EDGE];
let g = null; // gesture
const sideAllowed = (which) => {
  const r = route().name;
  return state.paired && !state.sheet && !dial && !state.item?.book?.picker && !state.item?.repairAsk && !state.item?.xlateAsk && !['pair', 'unsupported'].includes(r) && (which === 'left' || (r === 'item' && !!state.item?.meta));
};
function sidePanelEls() { return [app.querySelector('.side'), app.querySelector('.side-scrim')]; }
function paintDrag(p) {
  const [el, scrim] = sidePanelEls();
  if (!el) return;
  const w = el.offsetWidth;
  el.style.transition = 'none'; scrim.style.transition = 'none';
  el.style.transform = `translateX(${(g.which === 'left' ? -1 : 1) * (1 - p) * w}px)`;
  scrim.style.opacity = String(p);
}
function settleDrag(open) {
  const [el, scrim] = sidePanelEls();
  const which = g.which; g = null;
  if (!el) return;
  const ms = reduced() ? 0 : 180;
  el.style.transition = `transform ${ms}ms ease-out`; scrim.style.transition = `opacity ${ms}ms ease-out`;
  if (open) { el.style.transform = ''; scrim.style.opacity = ''; return; }
  el.style.transform = `translateX(${which === 'left' ? '-100%' : '100%'})`; scrim.style.opacity = '0';
  setTimeout(() => { if (state.side === which) closeSide(); }, ms);
}
const appX = (t) => ptX(t) - rectIn(app).left;
// Touch events keep going to the element the finger landed on, even after a re-render detached
// it from the page (then they no longer bubble to document): listen on that element too.
const seen = new WeakSet();
const once = (fn) => (e) => { if (seen.has(e)) return; seen.add(e); fn(e); };
function follow(target) {
  for (const [type, fn] of [['touchmove', onMove], ['touchend', endTouch], ['touchcancel', onCancel]]) {
    target.addEventListener(type, fn, { passive: false });
    setTimeout(() => target.removeEventListener(type, fn), 15000);
  }
}
document.addEventListener('touchstart', (e) => {
  g = null;
  if (e.target?.addEventListener) follow(e.target);
  if (e.touches.length !== 1) return;
  const t = e.touches[0]; const x = appX(t); const W = app.clientWidth;
  const base = { x0: ptX(t), y0: ptY(t), t0: performance.now(), x: ptX(t), y: ptY(t), rx: t.clientX, ry: t.clientY };
  if (dial) { if (dial.sticky && !e.target.closest?.('.dial')) shutDial(); return; } // the dial's own handlers turn it / pick a card
  if (performance.now() - dialShut < 400) return; // this touch only put the dial away
  if (state.side) { g = { ...base, mode: 'close', which: state.side }; return; }
  if (e.target.closest?.('.scrub')) {
    // The first 8 px decide (onMove 'scrub-wait'): from the thumb mostly leftward → the dial
    // (onMove 'scrub'); otherwise the thumb follows the finger ('thumb'). No movement: a tap.
    const th = rectIn(document.getElementById('scrub-thumb'));
    g = { ...base, mode: 'scrub-wait', thumb: lrOnThumb(base.y), mid: th ? (th.top + th.bottom) / 2 : base.y };
    document.getElementById('scrub')?.classList.add('held');
    return;
  }
  if (x >= LEFT_ZONE[0] && x <= LEFT_ZONE[1] && sideAllowed('left')) g = { ...base, mode: 'open', which: 'left' };
}, { passive: true });
for (const t of ['touchend', 'touchcancel']) document.addEventListener(t, () => document.getElementById('scrub')?.classList.remove('held'), { passive: true });
// The whole document is pinned: a touch that would move nothing scrollable is not allowed to
// drag the page (iOS rubber band); scroll panes, the chat list, tables and fields still scroll.
function scrollsUnder(el) {
  for (; el && el !== document.body; el = el.parentElement) {
    if (/^(TEXTAREA|INPUT|SELECT)$/.test(el.tagName)) return true;
    const cs = getComputedStyle(el);
    if (/(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 1) return true;
    if (/(auto|scroll)/.test(cs.overflowX) && el.scrollWidth > el.clientWidth + 1) return true;
  }
  return false;
}
const onMove = once((e) => {
  const t = e.touches[0];
  if (!g) { if (e.cancelable && !scrollsUnder(e.target)) e.preventDefault(); return; }
  g.x = ptX(t); g.y = ptY(t); g.rx = t.clientX; g.ry = t.clientY;
  const dx = g.x - g.x0; const dy = g.y - g.y0;
  if (g.mode === 'scrub-wait') {
    e.preventDefault();
    if (Math.hypot(dx, dy) < 8) return;
    if (g.thumb && dx < 0 && Math.abs(dx) > Math.abs(dy) * 1.2) { g = { ...g, mode: 'scrub' }; openDial(g.y0, false); }
    else if (Math.abs(dy) >= Math.abs(dx)) g = { ...g, mode: 'thumb', off: g.thumb ? g.y0 - g.mid : 0 };
    else { g = null; return; }
  }
  if (g.mode === 'thumb') { e.preventDefault(); lrScrubTo(g.y - g.off); return; }
  if (g.mode === 'scrub') { e.preventDefault(); if (!dial) return; dial.pull = Math.min(1, Math.max(0, -dx / DIAL_PULL)); dial.y = g.y; paintDial(); return; }
  if (!g.locked) {
    if (Math.hypot(dx, dy) < 8) return;
    const dirOk = g.mode === 'open' ? (g.which === 'left' ? dx > 0 : dx < 0) : (g.which === 'left' ? dx < 0 : dx > 0);
    if (!(dirOk && Math.abs(dx) > Math.abs(dy) * 1.2)) { g = null; return; } // vertical, or the wrong way: the page keeps it
    g.locked = true;
    if (g.mode === 'open') { state.side = g.which; state.sideShown = g.which; render(); }
  }
  e.preventDefault();
  const w = sidePanelEls()[0]?.offsetWidth || 1;
  g.p = g.mode === 'open' ? Math.min(1, Math.abs(dx) / w) : Math.max(0, 1 - Math.abs(dx) / w);
  g.vx = dx / Math.max(1, performance.now() - g.t0);
  paintDrag(g.p);
});
document.addEventListener('touchmove', onMove, { passive: false });
const endTouch = once((e) => {
  if (!g) return;
  if (g.mode === 'scrub') {
    e.preventDefault();
    // Pulled out far enough: the dial stays open, the level card marked, nothing jumps; a tap on a
    // card jumps there, a tap anywhere else puts it away. Released short of that: it goes away.
    const keep = dial && dial.pull >= DIAL_KEEP;
    g = null;
    if (!keep) { closeDial(); return; }
    dial.pull = 1; dial.sticky = true; dial.el.classList.add('sticky'); paintDial();
    return;
  }
  if (g.mode === 'thumb') { e.preventDefault(); g = null; return; }
  if (g.mode === 'scrub-wait') { e.preventDefault(); const y = g.y; g = null; lrScrubTap(y); return; }
  if (!g.locked) { g = null; return; }
  const fling = Math.abs(g.vx || 0) > 0.5;
  const toward = g.mode === 'open' ? (g.which === 'left' ? g.vx > 0 : g.vx < 0) : (g.which === 'left' ? g.vx > 0 : g.vx < 0);
  settleDrag(fling ? toward : g.p > (g.mode === 'open' ? 0.33 : 0.67));
});
const onCancel = once(() => { if (g?.locked) settleDrag(g.mode === 'close'); else if (g?.mode === 'scrub') { g = null; closeDial(); } else g = null; });
document.addEventListener('touchend', endTouch, { passive: false });
document.addEventListener('touchcancel', onCancel);

// ---------- 锁定竖屏 (iPhone): keep the app upright when the phone lies sideways ----------
// A web page cannot stop the system from rotating (iOS Safari and home-screen apps have no
// screen.orientation.lock). With 锁定竖屏 on (default), a landscape phone gets the whole #app turned
// back by ±90° (sized to the portrait box), so it stays upright relative to the device. Every custom
// gesture and geometric comparison goes through vp() / rectIn() / ptX() / ptY(): the viewport point or
// rectangle mapped into the app's own (unrotated) coordinates; elementFromPoint keeps raw viewport
// coordinates. While a text field is focused the rotation is paused (normal landscape layout), so the
// keyboard and the caret stay usable.
const ROT_KEY = 'memex.m.lockPortrait';
const lockPortrait = () => localStorage.getItem(ROT_KEY) !== '0';
let ROT = 0; // degrees #app is turned: 0, -90 or 90
let rotPaused = false;
function vp(x, y) {
  if (!ROT) return { x, y };
  const dx = x - innerWidth / 2; const dy = y - innerHeight / 2;
  // inverse of rotate(ROT): ROT = 90 → (dy, -dx); ROT = -90 → (-dy, dx)
  const u = ROT === 90 ? dy : -dy; const v = ROT === 90 ? -dx : dx;
  return { x: u + app.offsetWidth / 2, y: v + app.offsetHeight / 2 };
}
const ptX = (e) => vp(e.clientX, e.clientY).x;
const ptY = (e) => vp(e.clientX, e.clientY).y;
// An element's (or a Range's) box in app coordinates (a quarter turn keeps it axis-aligned).
function rectIn(el) {
  if (!el) return undefined;
  const r = el.getBoundingClientRect();
  if (!ROT) return r;
  const p = vp(r.left, r.top); const q = vp(r.right, r.bottom);
  const left = Math.min(p.x, q.x); const right = Math.max(p.x, q.x); const top = Math.min(p.y, q.y); const bottom = Math.max(p.y, q.y);
  return { left, right, top, bottom, width: right - left, height: bottom - top, x: left, y: top };
}
const phoneLike = () => (matchMedia('(pointer: coarse)').matches || navigator.maxTouchPoints > 0) && Math.min(innerWidth, innerHeight) <= 500;
function applyRotation() {
  const landscape = matchMedia('(orientation: landscape)').matches && innerWidth > innerHeight;
  let rot = 0;
  if (lockPortrait() && landscape && phoneLike() && !rotPaused) {
    // iOS reports window.orientation (90: turned left, -90: turned right); screen.orientation where present.
    const angle = typeof window.orientation === 'number' ? (window.orientation + 360) % 360 : screen.orientation?.angle ?? 90;
    rot = angle === 270 ? 90 : -90; // notch on the left (90°) → turn back counter-clockwise
  }
  const size = rot ? `${innerHeight}x${innerWidth}` : '';
  if (rot === ROT && app.dataset.rot === size) return;
  ROT = rot; app.dataset.rot = size;
  const root = document.documentElement;
  root.classList.toggle('rot-lock', !!rot); root.classList.toggle('rot-l', rot === -90); root.classList.toggle('rot-r', rot === 90);
  if (rot) { app.style.width = `${innerHeight}px`; app.style.height = `${innerWidth}px`; app.style.transform = `translate(-50%, -50%) rotate(${rot}deg)`; }
  else { app.style.width = ''; app.style.height = ''; app.style.transform = ''; }
  chromeKey = null;
  if (route().name === 'item') requestAnimationFrame(() => { lrPlace(); updateReaderChrome(); });
}
function tryOrientationLock() {
  // Only where the browser offers it (not iOS); a refusal is ignored, never reported as locked.
  try { const p = lockPortrait() ? screen.orientation?.lock?.('portrait') : screen.orientation?.unlock?.(); p?.catch?.(() => {}); } catch { /* unsupported */ }
}
for (const ev of ['resize', 'orientationchange']) addEventListener(ev, () => requestAnimationFrame(applyRotation));
screen.orientation?.addEventListener?.('change', () => requestAnimationFrame(applyRotation));
const isField = (el) => !!el && (/^(TEXTAREA|INPUT|SELECT)$/.test(el.tagName) || el.isContentEditable);
document.addEventListener('focusin', (e) => { if (isField(e.target) && ROT) { rotPaused = true; applyRotation(); } });
document.addEventListener('focusout', () => setTimeout(() => { if (rotPaused && !isField(document.activeElement)) { rotPaused = false; applyRotation(); } }, 60));
function setLockPortrait(on) { localStorage.setItem(ROT_KEY, on ? '1' : '0'); tryOrientationLock(); applyRotation(); render(); }

// ---------- render ----------
function render() {
  stopImages();
  const r = route();
  const libScroll = document.getElementById('lib-scroll');
  if (libScroll && r.name !== 'library') state.lib.scroll = libScroll.scrollTop;
  const readerScroll = document.getElementById('reader-scroll');
  // Only the reader that shows this item may save its position (a new item starts at the top).
  if (readerScroll && state.item && readerScroll.dataset.item === state.item.id) {
    if (readerScroll.classList.contains('vert')) state.item.vscroll = readerScroll.scrollLeft; else state.item.scroll = readerScroll.scrollTop;
  }
  const bk = state.item?.book;
  if (bk?.scrolls) {
    for (const p of document.querySelectorAll('.book-pane[data-pane]')) {
      if (p.dataset.item !== state.item.id) continue;
      const v = p.classList.contains('vert'); const k = v ? `${p.dataset.pane}|v` : p.dataset.pane;
      bk.scrolls.delete(k); bk.scrolls.set(k, v ? p.scrollLeft : p.scrollTop);
    }
    for (const k of bk.scrolls.keys()) { if (bk.scrolls.size <= 4 * BOOK_KEEP) break; bk.scrolls.delete(k); }
  }
  let screen;
  if (r.name === 'unsupported') screen = renderUnsupported();
  else if (r.name === 'pair') screen = renderPair();
  else if (r.name === 'item') screen = renderItem(r.id);
  else if (r.name === 'chat') screen = renderChatTab();
  else if (r.name === 'tasks') screen = renderTasks();
  else if (r.name === 'settings') { screen = renderSettings(); paintCacheCount(); }
  else if (r.name === 'trash') screen = renderTrash();
  else screen = renderLibrary();
  if (r.name !== 'item') state.sheet = null;
  // Leaving the reader releases the item: reopening paints from the cache and revalidates.
  if (r.name !== 'item' && state.item) { if (!state.item.cache) cachePut(state.item); state.item = null; clearTimeout(itemPoll); }
  // Another article (or none): the previous cover's request stops and its object URL is revoked.
  if (coverView && coverView.it !== state.item) dropCover();
  if (state.side === 'right' && r.name !== 'item') state.side = null;
  if (!state.side) state.sideShown = null;
  const sideScroll = state.side && document.getElementById(`side-${state.side}`)?.scrollTop;
  const side = state.side === 'left' ? renderLeftSide() : state.side === 'right' ? renderRightSide() : [];
  // The chapter editor keeps its caret, selection and scroll across a re-render.
  const ce = document.querySelector('.ce-text');
  const ceView = ce && { s: ce.selectionStart, e: ce.selectionEnd, top: ce.scrollTop, focus: document.activeElement === ce };
  const dfTops = [...document.querySelectorAll('.df-pane')].map((p) => [p.dataset.df, p.scrollTop]);
  app.replaceChildren(screen, ...side, ...(state.sheet ? renderSheet() : []));
  for (const [k, top] of dfTops) { const pane = app.querySelector(`.df-pane[data-df="${k}"]`); if (pane) pane.scrollTop = top; }
  const ce2 = ceView && app.querySelector('.ce-text');
  if (ce2) { ce2.scrollTop = ceView.top; if (ceView.focus) { ce2.focus({ preventScroll: true }); ce2.setSelectionRange(ceView.s, ceView.e); ce2.scrollTop = ceView.top; } }
  if (sideScroll) document.getElementById(`side-${state.side}`).scrollTop = sideScroll;
  // Book panes get their positions back in the same task as the swap (never a frame later).
  // Restoring both panes is not a reader's scroll: synced 并排 ignores the scroll events it causes.
  if (bk?.scrolls && state.item?.book === bk) { bookSyncMute = performance.now() + 150; for (const p of app.querySelectorAll('.book-pane[data-pane]')) {
    if (!p.classList.contains('vert')) { p.scrollTop = bk.scrolls.get(p.dataset.pane) || 0; continue; }
    holdVert(p, bk.scrolls.get(`${p.dataset.pane}|v`) || 0);
  } }
  if (r.name === 'item') lrPlace();
  psTick();
  if (g?.locked) paintDrag(g.p ?? 0); // a background re-render keeps the panel under the finger
  dial = null; chromeKey = null;
  if (r.name === 'item') requestAnimationFrame(updateReaderChrome);
}
async function boot() {
  state.tasks = loadTasks();
  applyRotation(); tryOrientationLock();
  if (GATEWAY) {
    try {
      conn = await openConnection({ onChange: () => {
        if (conn.paired !== state.paired) { state.paired = conn.paired; render(); return; }
        // Channel back after being offline: the relay only admits the phone while the Mac leg is up,
        // so the header may say 已连接 again; reload the visible library from the first window.
        const wasOff = !state.online;
        if (conn.connected) state.online = true;
        paintLink();
        if (conn.connected && (wasOff || state.lib.error)) { state.lib.error = null; loadLibrary(true); }
        if (conn.connected) pollTasks();
      } });
      source = createGatewaySource({ channel: conn, onReconnect: (d) => { state.reconnecting = Math.max(0, state.reconnecting + d); paintLink(); } });
      state.paired = conn.paired;
      if (conn.scope) { await cacheOpen(conn.scope); await primeGroups(); }
    } catch (e) { state.bootError = e?.message || 'unsupported'; }
  }
  if (!GATEWAY) { await cacheOpen('demo'); await primeGroups(); }
  render();
  pollTasks();
}
boot();

// iOS Safari ignores user-scalable=no; block its pinch gesture events directly.
for (const type of ['gesturestart', 'gesturechange', 'gestureend']) {
  document.addEventListener(type, (event) => event.preventDefault(), { passive: false });
}

// Demo preview only: the Markdown renderer for the DOM tests (never in gateway mode).
if (!GATEWAY) {
  globalThis.__memexDemo = { calls: source.calls, flags: source.flags, source,
    // 条目封面: the thumbnail reader kept for a future tiled view (exercised by the DOM test only).
    coverThumbnails: async (ids) => [...(await readCoverThumbnails(ids))].map(([id, g]) => [id, g.cover.version, g.blob.size, g.blob.type]) };
  // 阅读记录 timer: a replaceable monotonic clock (shared with the synthetic Mac) and the timer state.
  globalThis.__memexPasses = { setClock(fn) { psClock = fn; source.setPassClock(fn); }, tick: psTick, flush: () => psFlush(true),
    timer: () => ({ seg: T.seg && { ...T.seg }, session: T.session && { ...T.session }, queue: T.queue.map((x) => ({ ...x })), lastAct: T.lastAct }) };
  globalThis.__memexMd = { mdDoc, splitBlocks, completeBlocks, countText, sourceKey, groupSources, typeLabel, byteIdx, titleParts, fmtDuration,
    // One raw block rendered with the given highlight items on one side ('o' | 't').
    markBlock: (raw, items, side = 'o') => h('div', {}, mdBlock(raw, { marks: marksFor(0, side, raw, items), side })) };
}
