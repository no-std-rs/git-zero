import json,pathlib,hashlib,os,datetime,subprocess,collections
OUT=pathlib.Path('/tmp/git-zero-fit-response-independent-review-supplement-01')
RAW=pathlib.Path('/dev/shm/git-zero-fit-response-correctness-20261018')
WT=pathlib.Path('/tmp/git-zero-hosting-fit-response')
SUP=pathlib.Path('/tmp/git-zero-fit-response-controls-supplement-01.json')
V2=pathlib.Path('/tmp/git-zero-fit-response-runner-v2-handoff.json')
INV=pathlib.Path('/tmp/git-zero-fit-response-raw-evidence-inventory-supplement-01.json')
s=json.load(open(SUP));v2=json.load(open(V2));inv=json.load(open(INV))
declared={};records={};errors=[]
def collect(v):
    if isinstance(v,dict):
        if isinstance(v.get('path'),str) and v['path'].startswith('/') and 'sha256'in v:declared[v['path']]=v
        for x in v.values():collect(x)
    elif isinstance(v,list):
        for x in v:collect(x)
for j in [s,v2,inv]:collect(j)
for path,digest in {
str(SUP):'d8f3471297f05543a448154df2b3448f580190397bd882b149dc3e09f628da33',
str(SUP.with_suffix('.md')):'b04f0fbb6b6efa2337df7de83a7601abecb4075ac899707a0a04c075cd704b64',
str(V2):'ebb8460386f7698b6b0b4241f5b557965b60198e5ab7498c009bbcd972d5774a',
str(V2.with_suffix('.md')):'6c6031bce2b1d15789f376b3325b0388ad20944309bef842030d856cf4b1a590',
'/tmp/git-zero-fit-response-build-v2/manifest.json':'c34b3d1199ffbe208460df3be55d1a8690ca88c00611ced1cf0e2eafe0bb4b5c',
'/tmp/git-zero-fit-response-runner-v2-resources.json':'65589aeb8944e7829e0049187022d8b9a806b6fa59627f784297f727e4509696'
}.items():declared[path]={'sha256':digest}
def record(path):
    path=str(path)
    if path in records:return records[path]
    st=os.stat(path);h=hashlib.sha256()
    with open(path,'rb')as f:
        while b:=f.read(1024*1024):h.update(b)
    after=os.stat(path)
    if (st.st_size,st.st_mtime_ns)!=(after.st_size,after.st_mtime_ns):errors.append({'path':path,'error':'changed_during_hash'})
    r={'path':path,'bytes':after.st_size,'allocated_bytes':after.st_blocks*512,'mtime_ns':after.st_mtime_ns,'sha256':h.hexdigest()};records[path]=r;return r
for path,d in sorted(declared.items()):
    try:r=record(path)
    except FileNotFoundError:errors.append({'path':path,'error':'missing'});continue
    for k in ['sha256','bytes','allocated_bytes','mtime_ns']:
        if k in d and r[k]!=d[k]:errors.append({'path':path,'field':k,'expected':d[k],'actual':r[k]})
    if 'mtime_epoch'in d:
        sec,frac=d['mtime_epoch'].split('.');expected=int(sec)*1000000000+int(frac[:9].ljust(9,'0'))
        if r['mtime_ns']!=expected:errors.append({'path':path,'field':'mtime_epoch','expected_ns':expected,'actual_ns':r['mtime_ns']})
actual={str(p)for p in RAW.rglob('*')if p.is_file()};expected={x['path']for x in inv['files']}
inventory_set={'actual_files':len(actual),'declared_files':len(expected),'extra':sorted(actual-expected),'missing':sorted(expected-actual)}
raw_records=[r for r in records.values()if r['path'].startswith(str(RAW)+'/')]
raw_summary={'files':len(raw_records),'logical_bytes':sum(r['bytes']for r in raw_records),'allocated_bytes':sum(r['allocated_bytes']for r in raw_records),'inventory_set':inventory_set}
def load(p):return json.load(open(p))
allocation=load(RAW/'allocation-fit-v1-02/allocation-contract.json')
fault=load(RAW/'faults-fit-v1/fault-contract.json')
allocation_keys=list(allocation)
arows=allocation.get('pressure',allocation.get('rows',[]))
if not isinstance(arows,list):arows=[]
frows=fault['rows'];kindcounts=dict(collections.Counter(r['kind']for r in frows))
missing_patterns=['copied-final.*','contract.json','source-after.json','cleanup.json']
missing={p:[str(x)for x in (RAW/'faults-fit-v1').glob(p)]for p in missing_patterns};missing['resource-post']=[str(x)for x in RAW.glob('faults-fit-v1.resource-post.json')]
observer={ext:(RAW/'faults-fit-v1'/('bounded-final-object-stats.'+ext)).read_text()for ext in ['status','headers','body','exit','stderr']}
last=frows[-1];last_usage=last.get('after',last.get('follow'))['cloudflare_services']
native_log=(RAW/'faults-fit-v1/host.log').read_text()
provenance=[]
for scope in ['allocation-fit-v1-02','faults-fit-v1']:
    d=RAW/scope;before=load(d/'source-before.json');after=load(d/'source-after.json')if(d/'source-after.json').exists()else None
    sourceerrors=[]
    for name,val in before.items():
        if name=='tests/multi_chunk_cloudflare.rs':path=pathlib.Path('/tmp/git-zero-fit-response-build-v1/runner.rs')
        elif name=='runner_binary':path=pathlib.Path('/tmp/git-zero-fit-response-build-v1/runner-elf')
        elif 'path'in val:path=pathlib.Path(val['path'])
        else:path=WT/'examples/repository-host'/name
        r=record(path)
        if any(r[k]!=val[k]for k in ['bytes','sha256']):sourceerrors.append(name)
    runtimes=[]
    for p in sorted(d.glob('runtime-*.json')):
        procs=[]
        for pid,r in load(p).items():
            exe=pathlib.Path(r['exe']);kind='Native'if exe.name=='repository-host'else'runner'if exe.name.startswith('runner-elf')or exe.name.startswith('multi_chunk_cloudflare-')else'Node'if exe.name=='node'else'workerd'if exe.name=='workerd'else'ancillary'
            wanted=before.get({'Native':'native_binary','runner':'runner_binary','Node':'runtime_node','workerd':'runtime_workerd_binary'}.get(kind,''))
            ok=wanted is None or wanted['sha256']==r['elf_sha256']
            mapcount=sum(str(exe)in line for line in r['maps'].splitlines())
            env=r.get('actual_git_environment',{});clean=not('GIT_EDITOR'in env or 'GIT_TRACE2_EVENT'in env)if kind=='Native'else None
            procs.append({'pid':pid,'kind':kind,'exe':str(exe),'elf_sha256':r['elf_sha256'],'elf_matches':ok,'map_lines':mapcount,'Native_GIT_EDITOR_Trace2_absent':clean})
            if not ok or not mapcount or clean is False:errors.append({'scope':scope,'runtime':p.name,'pid':pid,'error':'runtime provenance'})
        runtimes.append({'path':str(p),'processes':procs})
    if sourceerrors:errors.append({'scope':scope,'error':'source_map_mismatch','names':sourceerrors})
    provenance.append({'scope':scope,'before_after_equal':before==after if after is not None else None,'source_after_present':after is not None,'source_map_mismatches':sourceerrors,'runtime':runtimes})
for d in ['v1','v2']:
    for name in ['native-elf','runner-elf','native.rs','cli.rs','runner.rs','manifest.json']:record(pathlib.Path('/tmp/git-zero-fit-response-build-'+d)/name)
record('/tmp/git-zero-fit-response-target-j2/debug/deps/multi_chunk_cloudflare-dcddae24be8f3d8c')
for rel in ['examples/repository-host/tests/multi_chunk_cloudflare.rs','examples/repository-host/src/native.rs','examples/repository-host/src/bin/repository-host.rs','examples/repository-host/src/accounting.rs','examples/repository-host/src/cost.rs']:
    record(WT/rel)
git=lambda *args:subprocess.run(['git','-c','core.pager=cat',*args],cwd=WT,check=True,capture_output=True,text=True).stdout
meta={'head':git('rev-parse','HEAD').strip(),'parent':git('rev-parse','HEAD^').strip(),'status':git('status','--porcelain=v1'),'native_tree':git('rev-parse','HEAD:examples/repository-host/src').strip(),'Worker_tree':git('rev-parse','HEAD:examples/repository-host/cloudflare/src').strip(),'changed_files':git('diff','--name-only','HEAD^','HEAD').splitlines()}
(OUT/'reviewed-runner.diff').write_text(git('diff','HEAD^','HEAD'))
summary={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'errors':errors,'raw':raw_summary,'source':meta,'allocation_contract_keys':allocation_keys,'allocation_pressure_rows':len(arows),'fault_kind_counts':kindcounts,'fault_row_count':len(frows),'fault_last_row_kind':last['kind'],'fault_last_usage':{'accounting_complete':last_usage['accounting_complete'],'accounting_errors':last_usage['accounting_errors'],'do_rows_read':last_usage['do_rows_read'],'do_rows_written':last_usage['do_rows_written']},'original_missing_receipts_present_matches':missing,'original_final_observer':observer,'Native_statistics_incomplete_log_present':'statistics failed: service accounting is incomplete'in native_log,'provenance':provenance,'supplement_first_admitted':s['allocation']['first_admitted'],'allocation_control_summary':s['allocation']['pressure']if False else None}
(OUT/'evidence-audit.json').write_text(json.dumps(summary,indent=2)+'\n')
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps({'audit_errors':errors,'files':sorted(records.values(),key=lambda r:r['path'])},indent=2)+'\n')
(OUT/'INPUT_SHA256SUMS').write_text(''.join(r['sha256']+'  '+r['path']+'\n'for r in sorted(records.values(),key=lambda r:r['path'])))
print(json.dumps({k:v for k,v in summary.items()if k not in ['provenance','fault_last_usage']},indent=2))
