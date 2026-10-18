// Test-only hooks around actual production Worker/DO/R2; Rust owns the runner.
import worker, { RepositoryServices as ProductionServices } from "../src/index.js";
import { manifestFingerprint, FETCH_AUTH } from "../src/first-read.js";
const originalFetch = globalThis.fetch;
const registrations = new Map();
let hook = null, activeEnv;
const encoder = new TextEncoder();
const hex = data => Array.from(data,b=>b.toString(16).padStart(2,"0")).join("");
async function resign(request, data, repo) {
  const config = registrations.get(repo);
  if (!config) throw new Error("test registration key unavailable");
  const key = await crypto.subtle.importKey("raw",Uint8Array.from(config.key.match(/../g),h=>parseInt(h,16)),{name:"HMAC",hash:"SHA-256"},false,["sign"]);
  const prefix = encoder.encode("git-zero-fetch-continue-v1\0"), input = new Uint8Array(prefix.length+data.length);
  input.set(prefix);input.set(data,prefix.length);
  const headers = new Headers(request.headers);headers.set(FETCH_AUTH,hex(new Uint8Array(await crypto.subtle.sign("HMAC",key,input))));
  return new Request(request.url,{method:"POST",headers,body:data});
}
globalThis.fetch = async (input,init) => {
  let request = input instanceof Request ? input : new Request(input,init);
  const path = new URL(request.url).pathname;
  if (!hook || hook.fired || !path.startsWith("/_git_zero_internal/")) return originalFetch(request);
  const planning = path.endsWith("/fetch-plan"), continuing = path.endsWith("/fetch-continue");
  if (planning && !["plan-auth","plan-response-auth","registry-loss"].includes(hook.mode)) {
    const incoming=new Uint8Array(await request.clone().arrayBuffer()),incomingView=new DataView(incoming.buffer);hook.request_headers=JSON.parse(new TextDecoder().decode(incoming.subarray(20,20+incomingView.getUint32(8,true)))).request.headers;
    const response=await originalFetch(request);if(response.ok){const raw=new Uint8Array(await response.clone().arrayBuffer()),d=new DataView(raw.buffer);hook.planned=JSON.parse(new TextDecoder().decode(raw.subarray(20,20+d.getUint32(8,true))));}return response;
  }
  if (planning && ["plan-auth","plan-response-auth","registry-loss"].includes(hook.mode)) {
    hook.fired = true;
    if (hook.mode === "plan-auth") { const headers = new Headers(request.headers);headers.set(FETCH_AUTH,"0".repeat(64));return originalFetch(new Request(request,{headers})); }
    const response = await originalFetch(request);
    if (hook.mode === "registry-loss") {
      await worker.fetch(new Request("http://test/_services/unregister",{method:"POST",headers:{"x-hosting-token":"git-zero-local-services","x-hosting-repo":hook.repo},body:"{}"}),activeEnv);
      return response;
    }
    const headers = new Headers(response.headers);headers.delete(FETCH_AUTH);return new Response(response.body,{status:response.status,headers});
  }
  if (!continuing || !hook.mode.startsWith("continue-")) return originalFetch(request);
  hook.fired = true;
  if (hook.mode === "continue-drop") throw new Error("test dropped paid continuation before Native");
  if (hook.mode === "continue-auth") {const headers=new Headers(request.headers);headers.set(FETCH_AUTH,"0".repeat(64));return originalFetch(new Request(request,{headers}));}
  let data = new Uint8Array(await request.arrayBuffer());
  let view = new DataView(data.buffer), metaLength=view.getUint32(8,true), bodyLength=view.getUint32(12,true);
  const meta = JSON.parse(new TextDecoder().decode(data.subarray(20,20+metaLength)));
  if (hook.mode === "continue-cache-fill") {
    const headers = new Headers(meta.request.headers);headers.set("x-git-zero-internal-generation-witness",meta.witness);
    const filled = await originalFetch(new Request(new URL(meta.request.target,request.url),{method:"POST",headers,body:data.slice(20+metaLength,20+metaLength+bodyLength)}));
    if (!filled.ok) throw new Error("test real cache-fill fetch failed");await filled.arrayBuffer();
  } else if (hook.mode === "continue-cache-pressure") {
    if (!Array.isArray(hook.pressure) || hook.pressure.length > 128 * 1024) throw new Error("invalid bounded test pressure request");
    const headers = new Headers(meta.request.headers);
    // A distinct real request obtains its own ordinary fresh authority. Its
    // productive response can evict the slot before the paid continuation.
    const filled = await originalFetch(new Request(new URL(meta.request.target,request.url),{method:"POST",headers,body:Uint8Array.from(hook.pressure)}));
    if (!filled.ok) throw new Error("test real pressure fetch failed");await filled.arrayBuffer();
  } else if (hook.mode === "continue-local-push") {
    const pushed=await originalFetch(new Request(new URL("/repo.git/git-receive-pack",request.url),{method:"POST",headers:{"content-type":"application/x-git-receive-pack-request"},body:Uint8Array.from(hook.push)}));
    const result = new TextDecoder().decode(await pushed.arrayBuffer());
    if (!pushed.ok || !result.includes("unpack ok") || !result.includes("ok refs/heads/main")) throw new Error("test actual intervening Native push failed: "+result);
  } else if (hook.mode === "continue-corrupt-signed") data[data.length-1]^=1;
  else if (hook.mode === "continue-truncate-signed") data=data.slice(0,-1);
  else if (hook.mode === "continue-wrong-body-signed") data[20+metaLength+bodyLength-1]^=1;
  else if (["continue-wrong-repo-signed","continue-wrong-session-signed","continue-wrong-target-signed","continue-wrong-selection","continue-duplicate-signed"].includes(hook.mode)) {
    let json;
    if (hook.mode !== "continue-duplicate-signed") {
      if(hook.mode==="continue-wrong-repo-signed")meta.repo="another";
      if(hook.mode==="continue-wrong-session-signed")meta.session="ff".repeat(16);
      if(hook.mode==="continue-wrong-target-signed")meta.request.target="/repo.git/git-receive-pack";
      if(hook.mode==="continue-wrong-selection")meta.plan.selection="00".repeat(32);
      json=JSON.stringify(meta);
    }
    else json='{"version":1,'+JSON.stringify(meta).slice(1);
    const encoded=encoder.encode(json), replacement=new Uint8Array(data.length-metaLength+encoded.length);
    replacement.set(data.subarray(0,20));new DataView(replacement.buffer).setUint32(8,encoded.length,true);replacement.set(encoded,20);replacement.set(data.subarray(20+metaLength),20+encoded.length);data=replacement;
  }
  request = hook.mode==="continue-wrong-selection" ? new Request(request.url,{method:"POST",headers:request.headers,body:data}) : await resign(request,data,hook.repo);
  return originalFetch(request);
};
export default {
  async fetch(request,env) {
    activeEnv=env;
    const path=new URL(request.url).pathname;
    if(path==="/_services/test/first-read-arm" && request.headers.get("x-hosting-token")==="git-zero-local-services") {
      const body=await request.clone().json();hook={...body,repo:request.headers.get("x-hosting-repo"),fired:false};
    }
    if(path==="/_services/test/first-read-canonical" && request.headers.get("x-hosting-token")==="git-zero-local-services") return Response.json({fingerprint:await manifestFingerprint(await request.json())});
    const response=await worker.fetch(request,env);
    if(path==="/_services/register" && response.ok) {const body=await response.clone().json();if(body.gateway_first_read)registrations.set(body.repository,body.gateway_first_read);}
    if(path==="/_services/test/first-read-trace" && response.ok){const value=await response.json();return Response.json({...value,gateway_plan:hook?.planned??null,gateway_hook_fired:hook?.fired??false,logical_headers:hook?.request_headers??null});}
    return response;
  }
};
export class RepositoryServices extends ProductionServices {
  constructor(ctx,env) {
    const fault={mode:"",fired:false,active:false,trace:[]};
    const sql=new Proxy(ctx.storage.sql,{get(target,name){if(name==="exec")return (...args)=>{const cursor=target.exec(...args);return {toArray:()=>cursor.toArray(),get rowsRead(){return fault.active&&fault.mode==="unknown-sql"?undefined:cursor.rowsRead;},get rowsWritten(){return cursor.rowsWritten;}};};const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;}});
    const storage=new Proxy(ctx.storage,{get(target,name){if(name==="sql")return sql;const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;}});
    const context=new Proxy(ctx,{get(target,name){if(name==="storage")return storage;const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;}});
    const blobs=new Proxy(env.BLOBS,{get(target,name){if(name==="get")return async (...args)=>{
      if(fault.active&&!fault.fired&&["get-failure","get-body-failure","get-corrupt"].includes(fault.mode)){
        fault.fired=true;
        if(fault.mode==="get-failure")return target.get(Symbol("actual invalid R2 argument"));
        const object=await target.get(...args);
        return new Proxy(object,{get(original,field){if(field==="arrayBuffer")return async()=>{const buffer=await original.arrayBuffer();if(fault.mode==="get-body-failure")throw new Error("test interrupted actual body; consumed bytes unknown");const out=new Uint8Array(buffer);out[0]^=1;return out.buffer;};const value=Reflect.get(original,field);return typeof value==="function"?value.bind(original):value;}});
      }
      return target.get(...args);
    };const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;}});
    super(context,{...env,BLOBS:blobs});this.firstReadFault=fault;
  }
  async operation(name,callback,read,written,key) {
    if(this.firstReadFault && this.firstReadFault.trace.length<4096)this.firstReadFault.trace.push({name,key:key??null,route:this.firstReadFault.route??null});
    return super.operation(name,callback,read,written,key);
  }
  async handle(request) {
    const path=new URL(request.url).pathname,f=this.firstReadFault;f.route=path;
    if(path==="/_services/test/first-read-arm") {this.currentPhase="stats";this.bump("do_requests");const body=await request.json();f.mode=body.mode;f.fired=false;f.trace=[];return Response.json({armed:body.mode});}
    if(path==="/_services/test/first-read-trace") {this.currentPhase="stats";this.bump("do_requests");return Response.json({trace:f.trace,fired:f.fired,mode:f.mode});}
    if(path==="/_services/manifest/first-read") {
      f.active=true;
      try {
        if(f.mode==="generation-change"&&!f.fired){f.fired=true;const row=this.exec("SELECT manifest FROM repository_state WHERE id = 1")[0];const next=JSON.parse(row.manifest),generation=next.generation;next.generation++;await this.publishManifest(generation,next);}
        return await super.handle(request);
      } finally { f.active=false; }
    }
    return super.handle(request);
  }
  async fetch(request) {
    const response=await super.fetch(request);
    if(new URL(request.url).pathname==="/_services/manifest/first-read"&&this.firstReadFault.mode==="missing-receipt") {const headers=new Headers(response.headers);headers.delete("x-git-zero-service-usage");return new Response(response.body,{status:response.status,headers});}
    return response;
  }
}
