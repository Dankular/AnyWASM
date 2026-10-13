// Minimal static file server for local development: node web/serve.mjs [port]
import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import { extname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(
  process.env.ANYWASM_WEB_ROOT || fileURLToPath(new URL('.', import.meta.url)),
);
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.awb': 'application/octet-stream', '.css': 'text/css' };
const port = Number(process.argv[2] || 8080);

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  let p = decodeURIComponent(url.pathname).replace(/^[/\\]+/, '');
  if (!p || p.endsWith('/') || p.endsWith('\\')) p = join(p, 'index.html');
  p = normalize(p).replace(/^(\.\.[/\\])+/, '');
  try {
    const path = join(root, p);
    const file = await stat(path);
    if (!file.isFile()) throw new Error('not a file');
    const headers = {
      'content-type': types[extname(p)] || 'application/octet-stream',
      'accept-ranges': 'bytes',
      'content-length': file.size,
      'cache-control': 'no-store',
      // Cross-origin isolation enables SharedArrayBuffer for the future threaded build.
      'cross-origin-opener-policy': 'same-origin',
      'cross-origin-embedder-policy': 'require-corp',
    };
    const range = req.headers.range;
    if (range) {
      const match = /^bytes=(\d*)-(\d*)$/.exec(range);
      if (!match || range.includes(',')) {
        res.writeHead(416, { ...headers, 'content-range': `bytes */${file.size}` }).end();
        return;
      }
      const size = file.size;
      const suffix = match[1] === '';
      const requestedStart = suffix ? Math.max(0, size - Number(match[2])) : Number(match[1]);
      const start = requestedStart;
      const end = suffix || match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1);
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || start >= size || end < start) {
        res.writeHead(416, { ...headers, 'content-range': `bytes */${size}` }).end();
        return;
      }
      res.writeHead(206, {
        ...headers,
        'content-length': end - start + 1,
        'content-range': `bytes ${start}-${end}/${size}`,
      });
      if (req.method === 'HEAD') res.end();
      else createReadStream(path, { start, end }).pipe(res);
      return;
    }
    res.writeHead(200, headers);
    if (req.method === 'HEAD') res.end();
    else createReadStream(path).pipe(res);
  } catch {
    res.writeHead(404).end('not found');
  }
});
server.listen(port, () => console.log(`serving ${root} on http://localhost:${server.address().port}`));
