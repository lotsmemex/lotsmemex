// Loopback-only static server for the mobile demo. Usage: node mobile/web/serve.mjs [port]
// Expose it to a phone with a separate tunnel (e.g. `cloudflared tunnel --url http://127.0.0.1:<port>`).
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('.', import.meta.url));
const port = Number(process.argv[2] || process.env.PORT || 8790);
const types = { '.html': 'text/html; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.png': 'image/png', '.txt': 'text/plain; charset=utf-8', '.woff2': 'font/woff2' };
const allowed = new Set(['/index.html', '/app.mjs', '/app.css', '/demo-source.mjs', '/gateway-source.mjs', '/fixtures.mjs', '/i18n.mjs', '/brand.mjs', '/changelog.mjs',
  '/assets/icon-192.png', '/assets/apple-touch-icon.png', '/assets/memex-avatar.png', '/assets/memex-portrait.png',
  // 古籍阅读: the 简繁 converter, its OpenCC dictionaries and the 寒蝉拙楷 font (each fetched on first use).
  '/zhconv.mjs', '/assets/opencc/STCharacters.txt', '/assets/opencc/STPhrases.txt', '/assets/opencc/TSCharacters.txt', '/assets/opencc/TSPhrases.txt',
  '/assets/opencc/LICENSE-OpenCC.txt', '/assets/fonts/ChillZhuoKai.woff2', '/assets/fonts/OFL-ChillZhuoKai.txt']);

createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname));
  const file = path === '/' ? '/index.html' : path;
  if (req.method !== 'GET' || !allowed.has(file)) { res.writeHead(404).end('not found'); return; }
  try {
    const body = await readFile(join(root, file));
    res.writeHead(200, {
      'content-type': types[extname(file)],
      'cache-control': 'no-store',
      'referrer-policy': 'no-referrer',
      'x-content-type-options': 'nosniff',
      'content-security-policy': "default-src 'self'; img-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self'; connect-src 'self'; frame-ancestors 'none'",
    });
    res.end(body);
  } catch { res.writeHead(404).end('not found'); }
}).listen(port, '127.0.0.1', () => console.log(`memex mobile demo on http://127.0.0.1:${port}/`));
