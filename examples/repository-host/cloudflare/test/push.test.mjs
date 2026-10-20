import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { test } from "node:test";
import { entry, object, packet, pack, project, wire } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

async function git(directory, ...args) {
  const child = spawn("git", ["-c", "user.name=Worker test", "-c", "user.email=worker@example.test", "-c", "http.extraHeader=Authorization: Bearer " + TOKEN, ...args], {
    cwd: directory, timeout: 30000, killSignal: "SIGKILL", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" },
  });
  let stdout = "", stderr = "";
  child.stdout.on("data", (bytes) => { stdout += bytes; }); child.stderr.on("data", (bytes) => { stderr += bytes; });
  const status = await new Promise((resolve, reject) => { child.once("error", reject); child.once("exit", resolve); });
  assert.equal(status, 0, `${args.join(" ")}\n${stdout}\n${stderr}`);
  return stdout.trim();
}
async function json(response) { const text = await response.text(); assert.equal(response.status, 200, text); return JSON.parse(text); }
async function refs(server, namespace) { return (await json(await fetch(`${server.origin}/_test/${namespace}/refs`, { headers: headers() }))).refs.map((ref) => ({ name: Buffer.from(ref.name).toString(), oid: ref.oid })); }
function request(format, commands, atomic = true, packed = pack(format, [])) {
  const caps = `report-status-v2 object-format=${format ? "sha256" : "sha1"}${atomic ? " atomic" : ""}`;
  return Buffer.concat([...commands.map((command, i) => packet(`${command.old} ${command.new} ${command.name}${i ? "" : `\0${caps}`}\n`)), Buffer.from("0000"), packed]);
}

for (const [runtime, serve] of [["Wrangler R2/SQLite", start], ["direct workerd with S3/SQLite", startStandalone]]) test(`ordinary Git pushes, clones and fetches interoperate with ${runtime}`, { timeout: 120000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), "git-zero-push-"));
  let server;
  try {
    server = await serve(join(root, "storage"));
    for (const format of [0, 1]) {
      const namespace = `git-${format}-${randomUUID()}`, directory = join(root, namespace);
      await mkdir(directory);
      const remote = `${server.origin}/${namespace}.git`;
      await json(await fetch(`${server.origin}/_test/${namespace}/initialize`, { method: "POST", headers: headers(format) }));
      await git(root, "clone", remote, join(root, `empty-${format}`));
      await git(directory, "init", "--initial-branch=main", `--object-format=${format ? "sha256" : "sha1"}`);
      const body = Array.from({ length: 4000 }, (_, i) => `line ${i}: immutable shared content for delta reconstruction\n`).join("");
      await writeFile(join(directory, "file"), body);
      await git(directory, "add", "file"); await git(directory, "commit", "-m", "initial");
      const first = await git(directory, "rev-parse", "HEAD");
      await git(directory, "-c", "protocol.version=0", "push", "--atomic", "--push-option=ci.skip", remote, "main");
      assert.deepEqual(await refs(server, namespace), [{ name: "refs/heads/main", oid: first }]);
      const clones = [];
      for (const version of [0, 1, 2]) {
        const clone = join(root, `clone-${format}-${version}`);
        await git(root, "-c", `protocol.version=${version}`, "clone", remote, clone);
        assert.equal(await git(clone, "rev-parse", "HEAD"), first);
        clones.push({ directory: clone, version });
      }
      await writeFile(join(directory, "file"), body.replace("line 2000:", "changed line 2000:"));
      await git(directory, "commit", "-am", "incremental");
      const second = await git(directory, "rev-parse", "HEAD");
      await git(directory, "-c", "protocol.version=2", "-c", "http.postBuffer=1", "push", "--atomic", remote, "main");
      assert.deepEqual(await refs(server, namespace), [{ name: "refs/heads/main", oid: second }]);
      const state = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      const stage = state.stages.find((stage) => stage.phase === "published");
      const staged = await json(await fetch(`${server.origin}/_test/${namespace}/stage/${stage.id}`, { headers: headers() }));
      const thin = staged.objects.find((row) => row.wire_kind === 7 && row.base_stage !== stage.id && row.base_oid);
      assert.ok(thin, "incremental Git push must exercise an external published delta base");
      const reconstructed = await fetch(`${server.origin}/_test/${namespace}/object/${stage.id}?node=${thin.node}&data=1`, { headers: headers() });
      assert.equal(reconstructed.status, 200);
      assert.equal(Buffer.from(await reconstructed.arrayBuffer()).toString(), body.replace("line 2000:", "changed line 2000:"));
      for (const clone of clones) {
        await git(clone.directory, "-c", `protocol.version=${clone.version}`, "fetch", "origin");
        assert.equal(await git(clone.directory, "rev-parse", "origin/main"), second);
        await git(clone.directory, "merge", "--ff-only", "origin/main");
        await git(clone.directory, "fsck", "--full", "--strict");
      }
      await git(directory, "tag", "--annotate", "annotated", "--message", "tag target");
      await git(directory, "push", remote, "refs/tags/annotated");
      await git(directory, "tag", "--annotate", "peeled-only", first, "--message", "older target without a direct ref");
      await git(directory, "push", remote, "refs/tags/peeled-only");
      const beforeWants = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      const repeated = 1024;
      const peeledControl = Buffer.concat([packet(`want ${first} side-band-64k object-format=${format ? "sha256" : "sha1"}\n`), ...Array.from({ length: repeated - 1 }, () => packet(`want ${first}\n`)), Buffer.from("0000"), packet("done\n")]);
      const peeledResponse = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request" }, body: peeledControl });
      assert.equal(peeledResponse.status, 200); await peeledResponse.arrayBuffer();
      const afterWants = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      assert.ok(afterWants.usage.sqlCalls - beforeWants.usage.sqlCalls < repeated + 100, "duplicate peeled wants must use indexed authorization, without rescanning refs");
      await git(directory, "push", remote, ":refs/tags/peeled-only");
      const revoked = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request" }, body: peeledControl });
      assert.equal(revoked.status, 400, "deleting the only advertising tag must revoke its peeled want"); await revoked.arrayBuffer();
      for (const version of [0, 1, 2]) {
        const listing = await git(root, "-c", `protocol.version=${version}`, "ls-remote", remote, "refs/tags/annotated*");
        assert.ok(listing.includes(`${second}\trefs/tags/annotated^{}`), `protocol ${version} must advertise peeled tag IDs`);
      }
      const fresh = join(root, `fresh-${format}`);
      await git(root, "-c", "protocol.version=2", "clone", remote, fresh);
      await git(fresh, "fsck", "--full", "--strict");
      assert.equal(await git(fresh, "rev-parse", "annotated^{}"), second);
      await git(directory, "push", remote, ":refs/tags/annotated");
      const control = Buffer.concat([packet(`want ${second} multi_ack_detailed side-band-64k object-format=${format ? "sha256" : "sha1"}\n`), Buffer.from("0000"), packet("done\n")]);
      const zipped = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "content-encoding": "gzip" }, body: gzipSync(control) });
      assert.equal(zipped.status, 200, await zipped.clone().text());
      assert.ok(Buffer.from(await zipped.arrayBuffer()).includes(Buffer.from("PACK")));
      await git(directory, "-c", "protocol.version=1", "push", "--atomic", remote, "main:refs/heads/topic", "main:refs/tags/release");
      assert.equal((await refs(server, namespace)).length, 3);
      await git(directory, "push", remote, ":refs/heads/topic", ":refs/tags/release");
      assert.deepEqual(await refs(server, namespace), [{ name: "refs/heads/main", oid: second }]);

      const zero = "0".repeat(first.length);
      const advert = Buffer.from(await (await fetch(`${remote}/info/refs?service=git-receive-pack`, { headers: headers() })).arrayBuffer());
      let refPacket;
      for (let at = 0; at < advert.length;) {
        const length = Number.parseInt(advert.subarray(at, at + 4).toString(), 16);
        if (length > 4 && advert.subarray(at + 4, at + length).includes(Buffer.from(" refs/heads/main\0"))) refPacket = length;
        at += Math.max(4, length);
      }
      assert.ok(refPacket);
      const headNamespace = `head-limit-${format}-${randomUUID()}`, headRemote = `${server.origin}/${headNamespace}.git`;
      const headName = "refs/heads/" + "a".repeat(65520 - refPacket + "refs/heads/main".length - "refs/heads/".length);
      const fixture = project(format);
      await json(await fetch(`${server.origin}/_test/${headNamespace}/initialize`, { method: "POST", headers: headers(format) }));
      const headResponse = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: fixture.root.id.toString("hex"), name: "refs/tags/carrier" }, { old: zero, new: fixture.root.id.toString("hex"), name: headName }], true, fixture.pack) });
      assert.equal(headResponse.status, 200); assert.ok((await headResponse.text()).includes(`ok ${headName}`));
      // The largest receive-compatible branch becomes symbolic HEAD. Ordinary
      // discovery must fit too, with exactly the host-backed upload capabilities.
      for (const version of [0, 1, 2]) assert.ok((await git(root, "-c", `protocol.version=${version}`, "ls-remote", "--symref", headRemote, "HEAD")).includes(`ref: ${headName}\tHEAD`));

      const commands = [{ old: first, new: second, name: "refs/heads/main" }, { old: zero, new: second, name: "refs/heads/accepted" }];
      let response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, commands) });
      assert.match(await response.text(), /ng refs\/heads\/accepted atomic push failed/);
      assert.equal((await refs(server, namespace)).length, 1);
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, commands, false) });
      assert.match(await response.text(), /ok refs\/heads\/accepted/);
      assert.equal((await refs(server, namespace)).length, 2);

      const overlong = "refs/heads/" + "a".repeat(65375 - "refs/heads/".length);
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: second, name: "refs/heads/z-guard" }, { old: zero, new: second, name: overlong }]) });
      assert.equal(response.status, 400); await response.text();
      assert.equal((await refs(server, namespace)).length, 2, "an unadvertisable late ref must not publish any command");
      await git(directory, "push", remote, "main");

      const duplicateNamespace = `identity-${format}-${randomUUID()}`;
      await json(await fetch(`${server.origin}/_test/${duplicateNamespace}/initialize`, { method: "POST", headers: headers(format) }));
      const duplicateRemote = `${server.origin}/${duplicateNamespace}.git/git-receive-pack`;
      const value = object(format, 3, "hello");
      response = await fetch(duplicateRemote, { method: "POST", headers: headers(), body: wire(format, pack(format, [entry(3, value.body)]), value.id) });
      assert.equal(response.status, 200); await response.arrayBuffer();
      // A valid delta can reconstruct exactly its external base ID. Its stored
      // physical base must survive publication and a new request-local cache.
      response = await fetch(duplicateRemote, { method: "POST", headers: headers(), body: wire(format, pack(format, [entry(7, Buffer.from([5, 5, 0x90, 5]), value.id)]), value.id, "refs/tags/identity") });
      assert.equal(response.status, 200); await response.arrayBuffer();
      const duplicateState = await json(await fetch(`${server.origin}/_test/${duplicateNamespace}/state`, { headers: headers() }));
      assert.equal(duplicateState.objects, 1);
      const duplicateStage = duplicateState.stages.find((stage) => stage.phase === "published");
      const duplicateRead = await fetch(`${server.origin}/_test/${duplicateNamespace}/object/${duplicateStage.id}?node=12&data=1`, { headers: headers() });
      assert.equal(duplicateRead.status, 200);
      assert.equal(await duplicateRead.text(), "hello");

      const collision = [{ old: zero, new: second, name: "refs/heads/main/topic" }];
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, collision) });
      assert.match(await response.text(), /ref name conflicts/);
      const races = await Promise.all([0, 1].map(() => fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: second, new: first, name: "refs/heads/main" }]) }).then((response) => response.text())));
      assert.equal(races.filter((response) => response.includes("ok refs/heads/main")).length, 1);
      assert.equal(races.filter((response) => response.includes("stale ref")).length, 1);
      await git(directory, "push", remote, "main");

      // A failed branch-kind check must not publish a valid staged blob.
      const blob = await git(directory, "rev-parse", "HEAD:file");
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: blob, name: "refs/heads/not-a-commit" }]) });
      assert.match(await response.text(), /branch target is not a commit/);
      const invalid = project(format, true);
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: invalid.wire });
      assert.equal(response.status, 200);
      assert.ok((await response.arrayBuffer()).byteLength > 0);
      assert.equal((await refs(server, namespace)).length, 2);
    }
    await server.stop(); server = await serve(join(root, "storage"));
    for (const name of await readdir(root)) {
      if (!name.startsWith("git-")) continue;
      const directory = join(root, name), remote = `${server.origin}/${name}.git`;
      const head = await git(directory, "rev-parse", "HEAD");
      assert.equal((await refs(server, name)).find((ref) => ref.name === "refs/heads/main").oid, head);
      await git(directory, "push", remote, "main:refs/heads/after-restart");
      assert.equal((await refs(server, name)).length, 3);
      const cold = join(root, `cold-${name}`);
      await git(root, "clone", "--bare", remote, cold);
      assert.equal(await git(cold, "rev-parse", "HEAD"), head);
      await git(cold, "fsck", "--full", "--strict");
    }
  } catch (error) { error.message += `\n${server?.logs() ?? ""}`; throw error; }
  finally { await server?.stop(); await rm(root, { recursive: true, force: true }); }
});
