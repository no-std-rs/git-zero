import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

export const execute = promisify(execFile);
export const oracle = fileURLToPath(new URL("../../../tests/server_revisions.py", import.meta.url));
export const module = await WebAssembly.compile(await readFile(new URL("../build/git_zero_worker_wasm.wasm", import.meta.url)));

export async function loadRepository(root, format) {
  const rows = (await readFile(join(root, "objects.txt"), "utf8")).trim().split("\n");
  const objects = await Promise.all(rows.map(async (row) => {
    const [id, kind] = row.split("\t");
    return { id, kind, body: await readFile(join(root, "raw", id)) };
  }));
  const refs = (await readFile(join(root, "refs.txt"), "utf8")).trim().split("\n").map((row) => {
    const [name, id, target, peeled] = row.split("\t");
    return { name, id, symbolicTarget: target === "-" ? null : target, peeled: peeled === "-" ? null : peeled };
  });
  const [id, target] = (await readFile(join(root, "head.txt"), "utf8")).split("\n");
  const head = /^0+$/.test(id) ? { type: "unborn", target } : { type: target ? "symbolic" : "detached", id, ...(target ? { target } : {}) };
  return { format, objects, refs, head };
}

export async function withFixture(format, callback) {
  const root = await mkdtemp(join(tmpdir(), `git-zero-worker-revisions-${format}-`));
  try {
    await execute("python3", [oracle, "prepare", "--root", root, "--object-format", format], { timeout: 60000 });
    const repository = await loadRepository(root, format);
    const ids = JSON.parse(await readFile(join(root, "revision-ids.json"), "utf8"));
    await callback({ root, repository, ids });
  } catch (error) {
    console.error(`Retained Worker revision fixture: ${root}`);
    throw error;
  }
  await rm(root, { recursive: true, force: true });
}

export function packet(line) {
  const bytes = Buffer.from(line);
  return Buffer.concat([Buffer.from((bytes.length + 4).toString(16).padStart(4, "0")), bytes]);
}

export function fetchBody(repository, version, arguments_, { id = repository.head.id, shallow = [], done = true } = {}) {
  const width = repository.format;
  const args = [
    ...(version === 2 ? [packet(`want ${id}\n`)] : [packet(`want ${id} shallow object-format=${width}\n`)]),
    ...shallow.map((id) => packet(`shallow ${id}\n`)),
    ...arguments_.map((line) => packet(line + "\n")),
  ];
  if (version === 2) {
    return Buffer.concat([packet("command=fetch\n"), packet(`object-format=${width}\n`), Buffer.from("0001"), ...args,
      ...(done ? [packet("done\n")] : []), Buffer.from("0000")]);
  }
  return Buffer.concat([...args, Buffer.from("0000"), ...(done ? [packet("done\n")] : [])]);
}

export function consume(exports, capacity = 17) {
  const parts = [];
  for (;;) {
    const count = exports.stream_read(capacity);
    if (count < 0) throw new Error(`stream_read: ${count}`);
    if (count === 0) return Buffer.concat(parts);
    parts.push(Buffer.from(new Uint8Array(exports.memory.buffer, exports.output_ptr(), count)));
  }
}
