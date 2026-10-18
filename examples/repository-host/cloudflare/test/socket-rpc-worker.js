// Test-only transport faults after real receipt-backed service work. Rust owns
// the runners, fixtures, assertions and retained source identities.
import inherited, { RepositoryServices as InheritedServices } from "./small-receive-worker.js";
let nonce = 0;
let fault = { mode: "", fired: false, nonce: "0", trace: [], physical_fired: false, epoch: "", seen: 0 };
const ARM_HEADER = "x-git-zero-test-socket-arm";
const TRACE_HEADER = "x-git-zero-test-socket-trace";
export class RepositoryServices extends InheritedServices {
  async handle(request) {
    const encoded=request.headers.get(ARM_HEADER);
    if(encoded) {
      const arm=JSON.parse(encoded);
      if(this.socketFaultNonce!==arm.nonce) {
        this.socketFaultNonce=arm.nonce;
        Object.assign(this.receiveFault,{mode:arm.mode,fired:false,active:false,puts:0,writes:0,deletes:0,trace:[]});
      }
    }
    const response=await super.handle(request);
    const f=this.receiveFault;
    const summary=JSON.stringify({nonce:this.socketFaultNonce,epoch:this.epoch,mode:f.mode,fired:f.fired,trace:f.trace});
    if(summary.length>60000)throw new Error("test trace exceeds bounded header");
    response.headers.set(TRACE_HEADER,summary);
    return response;
  }
}
const Pair = globalThis.WebSocketPair;
globalThis.WebSocketPair = class {
  constructor() {
    const pair = new Pair(), server = pair[1], send = server.send.bind(server);
    let route = "";
    server.addEventListener("message", event => {
      if (!(event.data instanceof ArrayBuffer)) return;
      const data = new Uint8Array(event.data);
      if (data.length < 20 || new TextDecoder().decode(data.subarray(0,4)) !== "GZSQ") return;
      try { const length = new DataView(data.buffer).getUint32(12,true);
        route = JSON.parse(new TextDecoder().decode(data.subarray(20,20+length))).route;
      } catch { /* Production validates before dispatch. */ }
    });
    server.send = data => {
      const bytes = new Uint8Array(data), magic = new TextDecoder().decode(bytes.subarray(0,4));
      if (route === "manifest/upload-retire" && !fault.fired && fault.mode.startsWith("socket-")) {
        if (magic === "GZSR" && fault.mode === "socket-lost-reply") {
          fault.fired=true; server.close(1011,"test lost committed socket reply"); throw new Error("test lost committed socket reply");
        }
        if (magic === "GZSR" && fault.mode === "socket-bad-id") {
          fault.fired=true;const changed=bytes.slice();new DataView(changed.buffer).setUint32(8,999,true);return send(changed);
        }
        if (magic === "GZSD" && fault.mode === "socket-duplicate-segment") {
          fault.fired=true;send(data);return send(data);
        }
        if (magic === "GZSE" && fault.mode === "socket-truncated") {
          fault.fired=true;server.close(1011,"test missing terminal");return;
        }
        if (magic === "GZSE" && fault.mode === "socket-bad-terminal") {
          fault.fired=true;const changed=bytes.slice();new DataView(changed.buffer).setUint32(16,999,true);return send(changed);
        }
      }
      return send(data);
    };
    return pair;
  }
};
export default {
  async fetch(request,env) {
    const path=new URL(request.url).pathname;
    if (request.headers.get("x-hosting-token")==="git-zero-local-services"
        && path==="/_services/test/small-receive-arm") {
      const body=await request.clone().json();fault={mode:body.mode,fired:false,nonce:String(++nonce),trace:[],physical_fired:false,epoch:"",seen:0};
    }
    // Test-only arm/trace continuity survives actual DO eviction. This wrapper
    // adds no storage operation and strips its private summary before the
    // production receipt validator/Native framing sees the response.
    const binding=env.REPOSITORIES;
    const repositories={idFromName:binding.idFromName.bind(binding),get(id){
      const stub=binding.get(id);
      return {async fetch(input){
        const headers=new Headers(input.headers);
        headers.set(ARM_HEADER,JSON.stringify({mode:fault.mode,nonce:fault.nonce}));
        const response=await stub.fetch(new Request(input,{headers}));
        const encoded=response.headers.get(TRACE_HEADER);
        if(encoded){
          const summary=JSON.parse(encoded);
          if(summary.nonce===fault.nonce){
            const start=summary.epoch===fault.epoch?fault.seen:0;
            if(summary.trace.length<start)throw new Error("test trace reset without epoch change");
            fault.trace.push(...summary.trace.slice(start));
            if(fault.trace.length>4096)throw new Error("test aggregate trace exceeds bound");
            fault.seen=summary.trace.length;fault.epoch=summary.epoch;
            fault.physical_fired ||= summary.fired;
          }
        }
        const clean=new Headers(response.headers);clean.delete(TRACE_HEADER);
        return new Response(response.body,{status:response.status,statusText:response.statusText,headers:clean});
      }};
    }};
    const response=await inherited.fetch(request,{...env,REPOSITORIES:repositories});
    if(path==="/_services/test/small-receive-trace"&&response.ok) {
      const body=await response.json();return Response.json({...body,mode:fault.mode,fired:fault.physical_fired,trace:fault.trace,
        puts:fault.trace.filter(row=>row.name==="r2_put").length,
        deletes:fault.trace.filter(row=>row.name==="r2_delete").length,
        socket_fault:{mode:fault.mode,fired:fault.fired}},{status:response.status,headers:response.headers});
    }
    return response;
  }
};
