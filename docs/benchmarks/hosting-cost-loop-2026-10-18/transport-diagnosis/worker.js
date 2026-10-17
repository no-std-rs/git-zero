const MAX_BODY = 64 * 1024 * 1024;
async function readBody(request) {
  const parts = []; let size = 0;
  const reader = request.body?.getReader();
  if (!reader) return new Uint8Array();
  for (;;) {
    const item = await reader.read();
    if (item.done) break;
    size += item.value.byteLength;
    if (size > MAX_BODY) throw new Error('body too large');
    parts.push(item.value);
  }
  const result = new Uint8Array(size); let offset = 0;
  for (const part of parts) { result.set(part, offset); offset += part.byteLength; }
  return result;
}
export default {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    const started = Date.now();
    const headers = new Headers(request.headers); headers.delete('host');
    const body = path.includes('buffer') ? await readBody(request) : request.body;
    console.log('proxy', path, 'body-ready-ms', Date.now() - started);
    const response = await fetch(new Request('http://127.0.0.1:39195/repo.git/git-receive-pack', { method: request.method, headers, body, redirect: 'manual' }));
    console.log('proxy', path, 'response-ms', Date.now() - started);
    return response;
  }
};
