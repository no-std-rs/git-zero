import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { test } from "node:test";
import { entry, object, packet, pack, project, wire } from "./fixture.mjs";
import { headers, start, TOKEN } from "./wrangler.mjs";
import { startStandalone } from "./standalone.mjs";

async function git(directory, ...args) {
  const child = spawn("git", ["-c", "user.name=Worker test", "-c", "user.email=worker@example.test", "-c", "http.followRedirects=false", "-c", "http.extraHeader=Authorization: Bearer " + TOKEN, ...args], {
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
  return Buffer.concat([...commands.map((command, i) => packet(Buffer.concat([Buffer.from(`${command.old} ${command.new} `), Buffer.from(command.name), Buffer.from(`${i ? "" : `\0${caps}`}\n`)]))), Buffer.from("0000"), packed]);
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
      await writeFile(join(directory, "small"), "small payload\n");
      await git(directory, "add", "file", "small"); await git(directory, "commit", "-m", "initial");
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
      const rewind = [{ old: second, new: first, name: "refs/heads/main" }, { old: "0".repeat(first.length), new: second, name: "refs/tags/rewind-side" }];
      for (const atomic of [true, false]) {
        const response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, rewind, atomic) });
        assert.equal(response.status, 200);
        const report = await response.text();
        assert.match(report, /ng refs\/heads\/main non-fast-forward/);
        assert.match(report, atomic ? /ng refs\/tags\/rewind-side atomic push failed/ : /ok refs\/tags\/rewind-side/);
        assert.equal((await refs(server, namespace)).find((ref) => ref.name === "refs/heads/main").oid, second);
      }
      await git(directory, "push", remote, ":refs/tags/rewind-side");
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
      for (const version of [0, 1, 2]) {
        const protocol = ["-c", `protocol.version=${version}`];
        const shallow = join(root, `shallow-${format}-${version}`);
        await git(root, ...protocol, "clone", "--depth=1", "--no-tags", remote, shallow);
        assert.equal(await git(shallow, "rev-list", "--count", "HEAD"), "1");
        assert.equal(await git(shallow, "rev-parse", "--is-shallow-repository"), "true");
        await git(shallow, ...protocol, "fetch", "--deepen=1", "origin");
        assert.equal(await git(shallow, "rev-list", "--count", "HEAD"), "2");
        await git(shallow, ...protocol, "fetch", "--unshallow", "origin");
        assert.equal(await git(shallow, "rev-parse", "--is-shallow-repository"), "false");
        await git(shallow, "fsck", "--full", "--strict");
        for (const [label, filter, depth, blobs] of [["none", "blob:none", false, 0], ["limit", "blob:limit=1024", false, 1], ["combined", "blob:none", true, 0]]) {
          const partial = join(root, `partial-${label}-${format}-${version}`);
          await git(root, ...protocol, "clone", `--filter=${filter}`, "--no-checkout", "--no-tags", ...(depth ? ["--depth=1"] : []), remote, partial);
          const kinds = (await git(partial, "cat-file", "--batch-all-objects", "--batch-check=%(objecttype)")).split("\n");
          assert.equal(kinds.filter((kind) => kind === "blob").length, blobs, `${label} must omit the requested blobs before checkout`);
          await git(partial, ...protocol, "checkout", "--detach", "HEAD");
          assert.equal(await readFile(join(partial, "file"), "utf8"), body.replace("line 2000:", "changed line 2000:"));
          assert.equal(await readFile(join(partial, "small"), "utf8"), "small payload\n");
          if (depth) {
            await git(partial, ...protocol, "fetch", "--deepen=1", "origin");
            assert.equal(await git(partial, "rev-list", "--count", "HEAD"), "2");
            await git(partial, ...protocol, "checkout", "--detach", "HEAD~1");
            assert.equal(await readFile(join(partial, "file"), "utf8"), body);
          }
          await git(partial, "fsck", "--full", "--strict");
        }
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
      assert.equal(afterWants.epoch, beforeWants.epoch, "query counters must belong to the same durable instance");
      assert.ok(afterWants.usage.sqlCalls - beforeWants.usage.sqlCalls < repeated + 100, "duplicate peeled wants must use indexed authorization, without rescanning refs");
      await git(directory, "push", remote, ":refs/tags/peeled-only");
      assert.equal(await git(root, "ls-remote", remote, "refs/tags/peeled-only*"), "");
      const privateBlob = object(format, 3, `private quarantine ${randomUUID()}`);
      await json(await fetch(`${server.origin}/_test/${namespace}/intake`, { method: "POST", headers: headers(format), body: wire(format, pack(format, [entry(3, privateBlob.body)]), privateBlob.id) }));
      const privateControl = Buffer.concat([packet(`want ${privateBlob.id.toString("hex")} side-band-64k object-format=${format ? "sha256" : "sha1"}\n`), Buffer.from("0000"), packet("done\n")]);
      const rejectedPrivate = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request" }, body: privateControl });
      assert.equal(rejectedPrivate.status, 400, "published-object grants must not include a retained quarantine"); await rejectedPrivate.arrayBuffer();
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
      for (const coding of ["gzip", "GZip", "GZIP", "x-gzip", "identity", "Identity", "IDENTITY"]) {
        const response = await fetch(`${remote}/git-upload-pack`, { method: "POST", headers: { ...headers(), "content-type": "application/x-git-upload-pack-request", "content-encoding": coding }, body: coding.toLowerCase().endsWith("gzip") ? gzipSync(control) : control });
        assert.equal(response.status, 200, `${coding}: ${await response.clone().text()}`);
        assert.ok(Buffer.from(await response.arrayBuffer()).includes(Buffer.from("PACK")));
      }
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
      const fixture = project(format);
      await json(await fetch(`${server.origin}/_test/${headNamespace}/initialize`, { method: "POST", headers: headers(format) }));
      const emptyAdvert = Buffer.from(await (await fetch(`${headRemote}/info/refs?service=git-upload-pack`, { headers: headers() })).arrayBuffer());
      let emptyPacket;
      for (let at = 0; at < emptyAdvert.length;) {
        const length = Number.parseInt(emptyAdvert.subarray(at, at + 4).toString(), 16);
        if (length > 4 && emptyAdvert.subarray(at + 4, at + length).includes(Buffer.from(" capabilities^{}\0"))) emptyPacket = length;
        at += Math.max(4, length);
      }
      assert.ok(emptyPacket);
      const limit = Math.min(65520 - refPacket, 65520 - emptyPacket) + "refs/heads/main".length;
      const headName = "refs/heads/" + "a".repeat(limit - "refs/heads/".length);
      const headResponse = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: fixture.root.id.toString("hex"), name: "refs/tags/carrier" }, { old: zero, new: fixture.root.id.toString("hex"), name: headName }], true, fixture.pack) });
      assert.equal(headResponse.status, 200); assert.ok((await headResponse.text()).includes(`ok ${headName}`));
      // The largest branch fitting all current projections becomes HEAD.
      for (const version of [0, 1, 2]) assert.ok((await git(root, "-c", `protocol.version=${version}`, "ls-remote", "--symref", headRemote, "HEAD")).includes(`ref: ${headName}\tHEAD`));
      const beyondHead = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: fixture.root.id.toString("hex"), name: headName + "b" }]) });
      assert.equal(beyondHead.status, 400); await beyondHead.arrayBuffer();
      const deleteHead = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, ["refs/tags/carrier", headName].map((name) => ({ old: fixture.root.id.toString("hex"), new: zero, name })), true, Buffer.alloc(0)) });
      assert.equal(deleteHead.status, 200);
      const deletion = await deleteHead.text(); assert.ok(deletion.includes(`ok ${headName}`)); assert.ok(deletion.includes("ok refs/tags/carrier"));
      assert.deepEqual(await refs(server, headNamespace), []);
      for (const version of [0, 1, 2]) await git(root, "-c", `protocol.version=${version}`, "ls-remote", headRemote);
      const carrier = "refs/tags/carrier", target = fixture.root.id.toString("hex");
      const createCarrier = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: target, name: carrier }]) });
      assert.equal(createCarrier.status, 200); assert.match(await createCarrier.text(), /ok refs\/tags\/carrier/);
      const tagAdvert = Buffer.from(await (await fetch(`${headRemote}/info/refs?service=git-upload-pack`, { headers: headers() })).arrayBuffer());
      let tagPacket;
      for (let at = 0; at < tagAdvert.length;) {
        const length = Number.parseInt(tagAdvert.subarray(at, at + 4).toString(), 16);
        if (length > 4 && tagAdvert.subarray(at + 4, at + length).includes(Buffer.from(` ${carrier}\0`))) tagPacket = length;
        at += Math.max(4, length);
      }
      assert.ok(tagPacket);
      const tagLimit = Math.min(65520 - tagPacket + carrier.length, 65520 - refPacket + "refs/heads/main".length);
      const tagName = "refs/tags/" + "a".repeat(tagLimit - "refs/tags/".length);
      const createTag = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: target, name: tagName }]) });
      assert.equal(createTag.status, 200); await createTag.arrayBuffer();
      const beyondTag = await fetch(`${headRemote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: target, name: tagName + "b" }]) });
      assert.equal(beyondTag.status, 400); await beyondTag.arrayBuffer();
      for (const version of [0, 1, 2]) await git(root, "-c", `protocol.version=${version}`, "ls-remote", headRemote);

      const beforeRefOnly = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      for (let attempt = 0; attempt < 16; attempt++) {
        const update = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: second, new: second, name: "refs/heads/main" }]) });
        assert.equal(update.status, 200); assert.match(await update.text(), /ok refs\/heads\/main/);
      }
      const afterRefOnly = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      assert.equal(afterRefOnly.stagedChunks, beforeRefOnly.stagedChunks, "ref-only transactions must not retain empty pack claims");
      assert.equal(afterRefOnly.stages.length, beforeRefOnly.stages.length);
      const unused = object(format, 3, `unused no-op pack ${randomUUID()}`);
      const unchanged = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: second, new: second, name: "refs/heads/main" }], true, pack(format, [entry(3, unused.body)])) });
      assert.equal(unchanged.status, 200); assert.match(await unchanged.text(), /ok refs\/heads\/main/);
      const absentDeletion = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: zero, name: "refs/heads/absent" }], true, Buffer.alloc(0)) });
      assert.equal(absentDeletion.status, 200); assert.match(await absentDeletion.text(), /ok refs\/heads\/absent/);
      const afterAbsent = await json(await fetch(`${server.origin}/_test/${namespace}/state`, { headers: headers() }));
      assert.equal(afterAbsent.objects, beforeRefOnly.objects);
      assert.equal(afterAbsent.stages.length, beforeRefOnly.stages.length);
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
      // A valid identity delta adds a ref without another canonical object.
      // Reclaim its pack while retaining the existing published base.
      response = await fetch(duplicateRemote, { method: "POST", headers: headers(), body: wire(format, pack(format, [entry(7, Buffer.from([5, 5, 0x90, 5]), value.id)]), value.id, "refs/tags/identity") });
      assert.equal(response.status, 200); await response.arrayBuffer();
      const duplicateState = await json(await fetch(`${server.origin}/_test/${duplicateNamespace}/state`, { headers: headers() }));
      assert.equal(duplicateState.objects, 1);
      assert.equal(duplicateState.stages.length, 1, "a ref update with only existing IDs must not retain another pack");
      assert.equal(duplicateState.stagedObjects, 1);
      assert.equal((await refs(server, duplicateNamespace)).length, 2);
      const duplicateStage = duplicateState.stages.find((stage) => stage.phase === "published");
      const duplicateRead = await fetch(`${server.origin}/_test/${duplicateNamespace}/object/${duplicateStage.id}?node=12&data=1`, { headers: headers() });
      assert.equal(duplicateRead.status, 200);
      assert.equal(await duplicateRead.text(), "hello");
      const copy = await fetch(duplicateRemote, { method: "POST", headers: headers(), body: wire(format, pack(format, [entry(3, value.body)]), value.id, "refs/tags/copy") });
      assert.equal(copy.status, 200); await copy.arrayBuffer();
      assert.equal((await refs(server, duplicateNamespace)).find((ref) => ref.name === "refs/tags/copy").oid, value.id.toString("hex"));
      const copiedState = await json(await fetch(`${server.origin}/_test/${duplicateNamespace}/state`, { headers: headers() }));
      assert.equal(copiedState.stages.length, 1);
      assert.equal(copiedState.stagedChunks, duplicateState.stagedChunks, "shared published chunk ownership must survive duplicate-pack cleanup");
      const identity = entry(7, Buffer.from([5, 5, 0x90, 5]), value.id);
      assert.ok(identity.length < 128);
      const changed = object(format, 3, "hello!");
      const mixedPack = pack(format, [identity, entry(6, Buffer.from([5, 6, 0x90, 5, 1, 33]), Buffer.from([identity.length]))]);
      const mixed = await fetch(duplicateRemote, { method: "POST", headers: headers(), body: wire(format, mixedPack, changed.id, "refs/tags/mixed") });
      assert.equal(mixed.status, 200); await mixed.arrayBuffer();
      assert.equal((await refs(server, duplicateNamespace)).find((ref) => ref.name === "refs/tags/mixed").oid, changed.id.toString("hex"));
      const mixedState = await json(await fetch(`${server.origin}/_test/${duplicateNamespace}/state`, { headers: headers() }));
      assert.equal(mixedState.objects, 2); assert.equal(mixedState.stages.length, 2);
      const mixedStage = mixedState.stages.find((stage) => stage.id !== duplicateStage.id);
      const mixedRead = await fetch(`${server.origin}/_test/${duplicateNamespace}/object/${mixedStage.id}?node=${12 + identity.length}&data=1`, { headers: headers() });
      assert.equal(mixedRead.status, 200); assert.equal(await mixedRead.text(), "hello!");
      const duplicateClone = join(root, `duplicate-clone-${format}`);
      await git(root, "clone", "--mirror", `${server.origin}/${duplicateNamespace}.git`, duplicateClone);
      await git(duplicateClone, "fsck", "--full", "--strict");
      assert.equal(await git(duplicateClone, "cat-file", "blob", "refs/tags/mixed"), "hello!");

      const manyNamespace = `many-refs-${format}-${randomUUID()}`, manyRemote = `${server.origin}/${manyNamespace}.git/git-receive-pack`;
      await json(await fetch(`${server.origin}/_test/${manyNamespace}/initialize`, { method: "POST", headers: headers(format) }));
      const manyFixture = project(format);
      const seedRefs = await fetch(manyRemote, { method: "POST", headers: headers(), body: manyFixture.wire });
      assert.equal(seedRefs.status, 200); await seedRefs.arrayBuffer();
      const beforeMany = await json(await fetch(`${server.origin}/_test/${manyNamespace}/state`, { headers: headers() }));
      const manyCommands = Array.from({ length: 16384 }, (_, i) => ({ old: zero, new: manyFixture.root.id.toString("hex"), name: `refs/tags/batch-${String(i).padStart(5, "0")}` }));
      const manyResponse = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, manyCommands) });
      assert.equal(manyResponse.status, 200); const manyReport = await manyResponse.text();
      assert.equal((manyReport.match(/ok refs\/tags\/batch-/g) ?? []).length, manyCommands.length);
      const afterMany = await json(await fetch(`${server.origin}/_test/${manyNamespace}/state`, { headers: headers() }));
      assert.equal(afterMany.refs, manyCommands.length + 1);
      const refReads = afterMany.usage.sqlRowsRead - beforeMany.usage.sqlRowsRead;
      assert.ok(refReads < manyCommands.length * 100, `batched ref conflict checks must use indexed prefixes: ${refReads} rows read`);
      const deepName = `refs/tags/${"x/".repeat(4096)}tip`;
      const deepResponse = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: manyFixture.root.id.toString("hex"), name: deepName }]) });
      assert.equal(deepResponse.status, 200); assert.ok(!(await deepResponse.text()).includes("ng "));
      const afterDeep = await json(await fetch(`${server.origin}/_test/${manyNamespace}/state`, { headers: headers() }));
      assert.equal(afterDeep.epoch, afterMany.epoch);
      const deepCalls = afterDeep.usage.sqlCalls - afterMany.usage.sqlCalls;
      assert.ok(deepCalls < 200, `deep ref must batch ancestor lookups: ${deepCalls} SQL calls`);
      const listedDeep = await git(root, "ls-remote", `${server.origin}/${manyNamespace}.git`, deepName);
      assert.equal(listedDeep, `${manyFixture.root.id.toString("hex")}\t${deepName}`);
      const rawPrefix = Buffer.concat([Buffer.from("refs/tags/raw-"), Buffer.from([255])]);
      const rawChild = Buffer.concat([rawPrefix, Buffer.from("/nested")]);
      const rawSibling = Buffer.concat([rawPrefix, Buffer.from(".before")]);
      const manyTarget = manyFixture.root.id.toString("hex");
      const rawCreate = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, [rawPrefix, rawSibling].map((name) => ({ old: zero, new: manyTarget, name }))) });
      assert.equal(rawCreate.status, 200); assert.ok(!(await rawCreate.text()).includes("ng "));
      const hiddenAncestor = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: manyTarget, name: rawChild }]) });
      assert.match(await hiddenAncestor.text(), /ref name conflicts/, "a lexicographically intervening sibling must not mask the ancestor");
      const replaceParent = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, [{ old: manyTarget, new: zero, name: rawPrefix }, { old: zero, new: manyTarget, name: rawChild }]) });
      assert.equal(replaceParent.status, 200); assert.ok(!(await replaceParent.text()).includes("ng "), "planned deletions must be removed before checking descendants");
      const descendant = await fetch(manyRemote, { method: "POST", headers: headers(), body: request(format, [{ old: zero, new: manyTarget, name: rawPrefix }]) });
      assert.match(await descendant.text(), /ref name conflicts/, "binary descendant ranges must reject recreating the ancestor");

      const collision = [{ old: zero, new: second, name: "refs/heads/main/topic" }];
      response = await fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, collision) });
      assert.match(await response.text(), /ref name conflicts/);
      await git(directory, "commit", "--allow-empty", "-m", "concurrent advance");
      const third = await git(directory, "rev-parse", "HEAD");
      const racingCommit = object(format, 1, `${await git(directory, "cat-file", "commit", third)}\n`);
      assert.equal(racingCommit.id.toString("hex"), third);
      const racingPack = pack(format, [entry(1, racingCommit.body)]);
      const races = await Promise.all([0, 1].map(() => fetch(`${remote}/git-receive-pack`, { method: "POST", headers: headers(), body: request(format, [{ old: second, new: third, name: "refs/heads/main" }], true, racingPack) }).then((response) => response.text())));
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
