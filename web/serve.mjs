// Minimal static file server for local development: node web/serve.mjs [port]
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('.', import.meta.url));
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.awb': 'application/octet-stream', '.css': 'text/css' };
const port = Number(process.argv[2] || 8080);

createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  let p = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
  if (p.endsWith('/')) p += 'index.html';
  try {
    const data = await readFile(join(root, p));
    res.writeHead(200, {
      'content-type': types[extname(p)] || 'application/octet-stream',
      // Cross-origin isolation enables SharedArrayBuffer for the future threaded build.
      'cross-origin-opener-policy': 'same-origin',
      'cross-origin-embedder-policy': 'require-corp',
    });
    res.end(data);
  } catch {
    res.writeHead(404).end('not found');
  }
}).listen(port, () => console.log(`serving ${root} on http://localhost:${port}`));
