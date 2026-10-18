// Test-only actual-binding faults; Rust owns all runners, fixtures and assertions.
import worker, { RepositoryServices as FirstReadServices } from "./first-read-worker.js";
const inheritedFetch = globalThis.fetch;
let gatewayFault = {mode:"",fired:false};
globalThis.fetch = async (input, init) => {
  const request = input instanceof Request ? input : new Request(input,init);
  const response = await inheritedFetch(request);
  if (gatewayFault.mode === "gateway-lost-reply" && !gatewayFault.fired && new URL(request.url).pathname.endsWith("/fetch-continue")) {
    gatewayFault.fired=true;gatewayFault.logical_output_bytes=(await response.arrayBuffer()).byteLength;throw new Error("test lost completed Native receive response");
  }
  return response;
};
export default {
  async fetch(request,env) {
    const path=new URL(request.url).pathname;
    if(path==="/_services/test/small-receive-arm" && request.headers.get("x-hosting-token")==="git-zero-local-services") {
      const body=await request.clone().json();gatewayFault={mode:body.mode,fired:false};
    }
    return worker.fetch(request,env);
  }
};
export class RepositoryServices extends FirstReadServices {
  constructor(ctx,env) {
    const fault={mode:"",fired:false,active:false,puts:0,writes:0,deletes:0,trace:[]};
    const fire=mode=>fault.active&&fault.mode===mode&&!fault.fired&&(fault.fired=true);
    const sql=new Proxy(ctx.storage.sql,{get(target,name){
      if(name==="exec")return(statement,...args)=>{
        const coupled=/^(INSERT INTO|UPDATE) repository_state/.test(statement)&&statement.includes("catalog_root");
        if(fault.puts&&coupled&&fire("sql-before"))throw new Error("test after actual PUT before coupled SQL");
        const cursor=target.exec(statement,...args);
        if(fault.active&&coupled)fault.writes++;
        const unknown=fault.puts&&coupled&&fire("unknown-sql");
        return {toArray:()=>cursor.toArray(),get rowsRead(){return unknown?undefined:cursor.rowsRead;},get rowsWritten(){return unknown?undefined:cursor.rowsWritten;}};
      };const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;
    }});
    const storage=new Proxy(ctx.storage,{get(target,name){
      if(name==="sql")return sql;
      if(name==="transactionSync")return callback=>target.transactionSync(()=>{const result=callback();if(fault.puts&&fault.writes&&fire("transaction"))throw new Error("test coupled transaction rollback");return result;});
      if(name==="sync")return async()=>{await target.sync();if(fault.puts&&fault.writes&&fire("sync-after"))throw new Error("test lost coupled durability sync reply");};
      const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;
    }});
    const context=new Proxy(ctx,{get(target,name){if(name==="storage")return storage;const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;}});
    const blobs=new Proxy(env.BLOBS,{get(target,name){
      if(name==="get")return async(...args)=>{
        const object=await target.get(...args);
        if(fault.mode==="suffix-get"&&!fault.fired&&fault.route==="/_services/blobs/read"){
          fault.fired=true;return new Proxy(object,{get(original,field){if(field==="arrayBuffer")return async()=>{await original.arrayBuffer();throw new Error("test lost actual suffix R2 body");};const value=Reflect.get(original,field);return typeof value==="function"?value.bind(original):value;}});
        }return object;
      };
      if(name==="put")return async(...args)=>{if(fault.active)fault.puts++;const result=await target.put(...args);if(fire("put-after"))throw new Error("test lost actual PUT reply");return result;};
      if(name==="delete")return async(...args)=>{if(fault.active&&++fault.deletes===2&&fire("partial-delete"))return target.delete(Symbol("actual invalid R2 DELETE argument"));return target.delete(...args);};
      const value=Reflect.get(target,name);return typeof value==="function"?value.bind(target):value;
    }});
    super(context,{...env,BLOBS:blobs});this.receiveFault=fault;
  }
  async operation(name,callback,read,written,key){
    if(this.receiveFault&&this.receiveFault.trace.length<4096)this.receiveFault.trace.push({name,key:key??null,route:this.receiveFault.route??null});
    return super.operation(name,callback,read,written,key);
  }
  async handle(request){
    const path=new URL(request.url).pathname,f=this.receiveFault;f.route=path;
    if(path==="/_services/test/small-receive-arm"){
      this.currentPhase="stats";this.bump("do_requests");const body=await request.json();Object.assign(f,{mode:body.mode,fired:false,active:false,puts:0,writes:0,deletes:0,trace:[]});return Response.json({armed:f.mode});
    }
    if(path==="/_services/test/small-receive-trace"){
      this.currentPhase="stats";this.bump("do_requests");return Response.json({trace:f.trace,fired:f.fired,puts:f.puts,writes:f.writes,deletes:f.deletes,mode:f.mode,gateway_fired:gatewayFault.fired,gateway_logical_output_bytes:gatewayFault.logical_output_bytes??null});
    }
    f.active=path==="/_services/manifest/upload-retire";
    try{
      const response=await super.handle(request);
      if(response.ok&&f.active&&f.mode==="lost-reply"&&!f.fired){f.fired=true;return Response.json({error:"test lost committed retirement reply"},{status:502});}
      return response;
    }finally{f.active=false;}
  }
}
