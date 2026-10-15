import { createServer } from "node:http";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

export async function withServer(fetch, callback) {
  const server = createServer(async (incoming, outgoing) => {
    try {
      const parts = [];
      for await (const part of incoming) parts.push(part);
      const url = `http://127.0.0.1:${server.address().port}${incoming.url}`;
      const init = { method: incoming.method, headers: incoming.headers };
      if (incoming.method === "POST") init.body = Buffer.concat(parts);
      const response = await fetch(new Request(url, init));
      outgoing.writeHead(response.status, Object.fromEntries(response.headers));
      await pipeline(Readable.fromWeb(response.body), outgoing);
    } catch (error) {
      if (outgoing.headersSent) outgoing.destroy(error);
      else { outgoing.writeHead(500); outgoing.end(String(error)); }
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    await callback(`http://127.0.0.1:${server.address().port}`);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}
