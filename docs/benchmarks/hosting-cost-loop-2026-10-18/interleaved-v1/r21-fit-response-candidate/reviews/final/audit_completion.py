import pathlib,json,hashlib,datetime,collections,subprocess,re,math
OUT=pathlib.Path('/tmp/git-zero-fit-response-independent-final-review')
ROOT=pathlib.Path('/tmp/git-zero-fit-response-correctness-manifests')
RAW='/dev/shm/git-zero-fit-response-correctness-20261018'
WT=pathlib.Path('/tmp/git-zero-hosting-fit-response');BASE=WT/'examples/repository-host'
load=lambda p:json.load(open(p))
inv=load(OUT/'INPUT_SHA256_INVENTORY.json');records={r['path']:r for r in inv['files']};errors=[]
float_roundtrips=[]
def receipt_equal(a,b,scope,path=''):
    if isinstance(a,dict)and isinstance(b,dict):
        return set(a)==set(b)and all(receipt_equal(a[k],b[k],scope,path+'.'+k)for k in a)
    if isinstance(a,list)and isinstance(b,list):
        return len(a)==len(b)and all(receipt_equal(x,y,scope,path+f'[{i}]')for i,(x,y)in enumerate(zip(a,b)))
    if a==b:return True
    if isinstance(a,float)and isinstance(b,float)and math.isfinite(a)and math.isfinite(b)and abs(a-b)<=max(math.ulp(a),math.ulp(b))and(path.endswith('_seconds')or'estimated_cost'in path):
        float_roundtrips.append({'scope':scope,'path':path,'raw_parsed_value':a,'enclosing_parsed_value':b,'absolute_difference':abs(a-b),'maximum_ulp':max(math.ulp(a),math.ulp(b))})
        return True
    return False
def check(ok,reason,**context):
    if not ok:errors.append({'reason':reason,**context})
def record(path):
    path=str(path)
    if path not in records:
        s=pathlib.Path(path).stat();h=hashlib.sha256()
        with open(path,'rb')as f:
            while b:=f.read(1024*1024):h.update(b)
        records[path]={'path':path,'bytes':s.st_size,'allocated_bytes':s.st_blocks*512,'mtime_ns':s.st_mtime_ns,'sha256':h.hexdigest()}
    return records[path]
def sha(path):return record(path)['sha256']
def raw_record(scope,name):return records[RAW+'/'+scope+'/'+name]
def http(scope,name,status):
    d=ROOT/scope;body=(d/(name+'.body')).read_bytes();code=(d/(name+'.status')).read_text();exit=(d/(name+'.exit')).read_text();stderr=(d/(name+'.stderr')).read_bytes()
    check(code==str(status),'HTTP status',scope=scope,name=name,actual=code,expected=status)
    check(exit=='Some(0)'and not stderr,'HTTP transport evidence',scope=scope,name=name)
    return body
def wire(scope,row,name=None):
    if'response_sha256'not in row:return None
    if name is None:name=row.get('name')
    if name is None:
        candidates=[r for p,r in records.items()if p.startswith(RAW+'/'+scope+'/')and r['sha256']==row['response_sha256']]
        check(bool(candidates),'wire raw digest absent',scope=scope)
        return {'row':row.get('kind'),'matching_raw_files':[r['path']for r in candidates]}
    r=raw_record(scope,name+'.body');check(r['sha256']==row['response_sha256'],'wire hash mismatch',scope=scope,name=name)
    if'response_bytes'in row:check(r['bytes']==row['response_bytes'],'wire length mismatch',scope=scope,name=name)
    return {'row':row.get('name',row.get('kind')),'body':name+'.body','sha256':r['sha256'],'bytes':r['bytes']}
def metrics_walk(v,out):
    if isinstance(v,dict):
        if'cache_retained_bytes'in v and'joint_cache_entries'in v:out.append(v)
        for x in v.values():metrics_walk(x,out)
    elif isinstance(v,list):
        for x in v:metrics_walk(x,out)
def bounds(v,scope):
    metrics=[];metrics_walk(v,metrics)
    for m in metrics:
        check(m['cache_retained_bytes']<=33554432 and m['joint_cache_retained_peak_bytes']<=33554432 and m['joint_cache_entries']<=256 and m['protected_chunk_entries']<=32,'cache bounds',scope=scope)
        check(m['cache_budget_bytes']==33554432 and m['cache_entry_limit']==256,'cache limits',scope=scope)
        if'native_socket_rpc'in m:check(m['native_socket_rpc']['retained_reservation_bytes']==262144,'socket cap',scope=scope)
    return {'metric_snapshots_checked':len(metrics),'max_retained_bytes':max((m['cache_retained_bytes']for m in metrics),default=0),'max_reported_retained_peak':max((m['joint_cache_retained_peak_bytes']for m in metrics),default=0),'max_entries':max((m['joint_cache_entries']for m in metrics),default=0)}
scopes=['faults-fit-v2-02','restart-fit-v2-01','warm-fit-v2-01','http-fit-v2-01']
scope_results=[];v2maps=[]
for scope in scopes:
    d=ROOT/scope;c=load(d/'contract.json');before=load(d/'source-before.json');after=load(d/'source-after.json');v2maps.append(before)
    check(before==after,'source before/after',scope=scope)
    final=load(d/'copied-final.body');check(receipt_equal(final,c['final'],scope,'.final'),'saved cumulative equals enclosing contract',scope=scope)
    manifest=load(d/'bounded-final-manifest.body');check(manifest==c['final_manifest'],'saved final manifest',scope=scope)
    http(scope,'bounded-final-manifest',200);http(scope,'copied-final',200)
    expected=503 if scope.startswith('faults')else 200;body=http(scope,'bounded-final-object-stats',expected);obs=c['final_object_observation']
    check(obs=={'status':expected,'response_bytes':len(body),'response_sha256':hashlib.sha256(body).hexdigest()},'final observation descriptor',scope=scope)
    order=['bounded-final-manifest.body','bounded-final-object-stats.body','copied-final.body','source-after.json','cleanup.json']
    stamps=[raw_record(scope,name)['mtime_ns']for name in order];check(stamps==sorted(stamps),'final observer/cumulative/closure order',scope=scope)
    cleanup=load(d/'cleanup.json');check(cleanup.get('owned_process_groups_stopped')is True and cleanup.get('owned_ports')==[8849,8850],'scope cleanup receipt',scope=scope)
    resources={}
    for stage in ['pre','post']:
        r=load(ROOT/(scope+'.resource-'+stage+'.json'));ok=r['MemAvailable']>=42949672960 and r['tmp_available']>=2147483648 and max(r['own_prefix_allocated_bytes'],r['own_prefix_logical_bytes'])+r['scope_forecast']<=8589934592
        check(ok,'scope resource guard',scope=scope,stage=stage);resources[stage]=r
    check(records[RAW+'/'+scope+'.resource-pre.json']['mtime_ns']<=raw_record(scope,'source-before.json')['mtime_ns'],'PRE precedes scope source',scope=scope)
    check(raw_record(scope,'cleanup.json')['mtime_ns']<=records[RAW+'/'+scope+'.resource-post.json']['mtime_ns'],'POST follows cleanup',scope=scope)
    wires=[wire(scope,r,'restarted-prepared-receive'if r.get('name')=='same-root-restarted-prepared-receive'else None)for r in c['rows']if'response_sha256'in r]
    result={'scope':scope,'cohort':c['cohort'],'rows':len(c['rows']),'names':[r.get('name',r.get('kind'))for r in c['rows']],'source_before_after_equal':before==after,'final_observer':obs,'closure_order':dict(zip(order,stamps)),'resources':resources,'bounds':bounds(c,scope),'wire':wires}
    usage=final['cloudflare_services']
    if scope.startswith('faults'):
        log=(d/'host.log').read_text().splitlines();reason='statistics failed: service accounting is incomplete';lines=[i+1 for i,x in enumerate(log)if x==reason]
        check(body==b'statistics failed'and usage['accounting_complete']is False and usage['do_rows_read']is None and usage['do_rows_written']is None,'fault unknown accounting remains unknown',scope=scope)
        check(c['final_object_stats']is None and c['final_estimated_cost']is None and c['final_object_stats_unavailable_reason']=='service accounting is incomplete','fault NULL result reason',scope=scope)
        check(bool(lines),'actual Native log reason',scope=scope)
        check(usage['operations']['r2_list']['attempts']>0 and usage['r2_live_bytes']>0 and usage['r2_live_objects']>0,'fault final physical LIST gauges retained',scope=scope)
        result['fault_final']={'Native_reason_lines':lines,'accounting_complete':False,'accounting_errors':usage['accounting_errors'],'SQL_read':None,'SQL_write':None,'object_stats':None,'estimated_cost':None,'physical_LIST':usage['operations']['r2_list'],'r2_live_bytes':usage['r2_live_bytes'],'r2_live_objects':usage['r2_live_objects']}
    else:
        obj=json.loads(body);check(receipt_equal(obj,c['final_object_stats'],scope,'.final_object_stats')and receipt_equal(obj['estimated_cost'],c['final_estimated_cost'],scope,'.final_estimated_cost'),'clean observer object/estimate receipts',scope=scope)
        check(usage['accounting_complete']is True and not usage['accounting_errors'],'clean accounting complete',scope=scope)
    if scope.startswith('restart'):
        empty=load(d/'same-root-restarted.body');r=c['rows'][1];check(empty==r['before'],'restart saved before equals row',scope=scope)
        check(empty['protected_chunk_entries']==0 and empty['protected_chunk_admissions']==0 and empty['cache_entries']==0,'restart empty process cache',scope=scope)
        delta={k:r['after'][k]-r['before'][k]for k in ['gateway_receive_consumed','protected_chunk_admissions']};check(delta['gateway_receive_consumed']==1 and delta['protected_chunk_admissions']==8,'restart actual prepared receive',scope=scope)
        result['restart']={'empty_cache':True,'prepared_delta':delta,'latest_history_verified_rows':result['names'][2:]}
    if scope.startswith('warm'):
        initial=c['initial']['cloudflare_services'];check(usage['r2_class_b']==initial['r2_class_b']and usage['r2_read_bytes']==initial['r2_read_bytes']and final['protected_chunk_admissions']==0,'warm zero added GET/admission',scope=scope)
        result['warm']={'startup_GET':initial['r2_class_b'],'final_GET':usage['r2_class_b'],'startup_read_bytes':initial['r2_read_bytes'],'final_read_bytes':usage['r2_read_bytes'],'admissions':final['protected_chunk_admissions']}
    if scope.startswith('http'):
        check(final['gateway_read_consumed']==0 and final['gateway_receive_consumed']==0 and final['protected_chunk_admissions']==11 and final['protected_chunk_hits']==65,'ordinary HTTP compatibility',scope=scope)
        result['http']={k:final[k]for k in ['gateway_read_consumed','gateway_receive_consumed','protected_chunk_admissions','protected_chunk_hits']}
    scope_results.append(result)
check(all(v==v2maps[0]for v in v2maps),'v2 source maps equal across scopes')
# Independently recompute every real fault attempt/followup, keeping exclusions explicit.
fault=load(ROOT/'faults-fit-v2-02/fault-contract.json');frows=fault['rows'];attempts=[];follow=[];read_modes=[];fwires=[]
for r in frows:
    if r['kind']=='publication attempt':
        cas=sum(x['name']=='manifest_cas'for x in r['trace']['trace']);check(not r['fired']or cas<=1,'no replayed fired CAS',mode=r['mode']);attempts.append({'mode':r['mode'],'attempt':r['attempt'],'fired':r['fired'],'status':r['status'],'manifest_cas_attempts':cas});fwires.append(wire('faults-fit-v2-02',r,'push-'+r['mode']+'-'+str(r['attempt'])))
    elif r['kind']=='same Native distinct next push':
        ops=r['trace']['trace'];firstput=next(i for i,x in enumerate(ops)if x['name']=='r2_put');gc=any(x['name']=='r2_list'and x['route']=='/_services/gc'for x in ops[:firstput]);excluded=r['mode']in ['unknown-sql','gateway-lost-reply'];check(gc or excluded,'required same Native GC before PUT',mode=r['mode']);follow.append({'mode':r['mode'],'GC_before_first_PUT':None if excluded else gc,'ordering_excluded':excluded,'actual_trace_has_GC_before_first_PUT':gc})
    elif r['kind']=='read fault':
        read_modes.append(r['mode']);fwires.append(wire('faults-fit-v2-02',r,'fetch-'+r['mode']))
    else:fwires.append(wire('faults-fit-v2-02',r))
fault_result={'rows':len(frows),'kinds':dict(collections.Counter(r['kind']for r in frows)),'read_modes':read_modes,'publication_attempts':attempts,'fired_modes':sorted({r['mode']for r in attempts if r['fired']}),'same_Native_followups':follow,'bounds':bounds(fault,'faults-fit-v2-02'),'wire':fwires}
check(len(frows)==38 and len(attempts)==14 and len(follow)==13 and len(read_modes)==10 and len(fault_result['fired_modes'])==13,'fault coverage counts')
# Source maps are checked against version-appropriate archives, not the changed final runner.
source_checks=[]
for d in sorted(ROOT.iterdir()):
    if not d.is_dir()or not(d/'source-before.json').exists():continue
    before=load(d/'source-before.json');after=load(d/'source-after.json')if(d/'source-after.json').exists()else None
    version='v0'if d.name.endswith('-v0')else'v2'if d.name in scopes else'v1'
    mismatch=[]
    for name,v in before.items():
        if name=='tests/multi_chunk_cloudflare.rs':path=pathlib.Path('/tmp/git-zero-fit-response-build-'+version)/'runner.rs'
        elif name=='runner_binary':path=pathlib.Path('/tmp/git-zero-fit-response-build-'+version)/'runner-elf'
        elif'path'in v:path=pathlib.Path(v['path'])
        else:path=BASE/name
        r=record(path)
        if r['bytes']!=v['bytes']or r['sha256']!=v['sha256']:mismatch.append(name)
    check(not mismatch,'versioned source map mismatch',scope=d.name,files=mismatch)
    if d.name!='faults-fit-v1':check(before==after,'completed source map closure',scope=d.name)
    source_checks.append({'scope':d.name,'version':version,'source_map_files':len(before),'mismatches':mismatch,'source_before_after_equal':before==after if after is not None else None})
runtime_index=load('/tmp/git-zero-fit-response-runtime-inventory-final.json');native={};runtime=[]
for snap in runtime_index['snapshots']:
    path=pathlib.Path(snap['snapshot']);actual=load(ROOT/path.parent.name/path.name);procs=[]
    for pid,v in actual.items():
        indexed=next((x for x in snap['processes']if str(x['pid'])==pid),None);check(indexed is not None,'runtime index PID omitted',path=str(path),pid=pid)
        for k in ['exe','elf_bytes','elf_sha256','cmdline','actual_git_environment','environment_sha256','is_native']:
            check(indexed is not None and indexed[k]==v[k],'runtime index differs from actual snapshot',path=str(path),pid=pid,field=k)
        exe=pathlib.Path(v['exe']);r=record(exe);check(r['sha256']==v['elf_sha256']and r['bytes']==v['elf_bytes'],'actual current/preserved runtime ELF continuity',exe=str(exe))
        maps=sum(str(exe)in line for line in v['maps'].splitlines());check(maps>0,'runtime executable map omitted',path=str(path),pid=pid)
        if v['is_native']:
            env=v['actual_git_environment'];canonical=env.get('GIT_PAGER')=='cat'and all(k=='GIT_PAGER'or k.startswith('GIT_ZERO_')for k in env);check(canonical,'Native Git environment not canonical',pid=pid);native[pid]=v
        procs.append({'pid':pid,'exe':str(exe),'elf_sha256':v['elf_sha256'],'map_lines':maps,'is_native':v['is_native']})
    runtime.append({'snapshot':str(path),'processes':procs})
check(len(runtime)==26 and len(native)==12,'runtime snapshot/Native process count',snapshots=len(runtime),Native=len(native))
restart_native=[]
for name in ['runtime-ready-host.log.json','runtime-ready-host-restarted.log.json']:
    v=next(v for v in load(ROOT/'restart-fit-v2-01'/name).values()if v['is_native']);restart_native.append({'snapshot':name,'cmdline':v['cmdline'],'root':v['cmdline'].split()[-2]})
check(restart_native[0]['root']==restart_native[1]['root']==RAW+'/restart-fit-v2-01/native','restart identical Native root')
index_path='/tmp/git-zero-fit-response-final-evidence-index.json';check(sha(index_path)=='3496c261934d5f708ae581b708cf15f4a3e80fd243c8d493c79758ace12f38bf','standalone named index SHA');check(load(index_path)==load('/tmp/git-zero-fit-response-completion-handoff.json')['inventories']['named_evidence_index'],'standalone and embedded evidence index equality')
# Preserve earlier independent review outputs exactly, rather than rewrite history.
prior={
'/tmp/git-zero-fit-response-independent-review/REVIEW.md':'5c8f7db1ba8a7c3b381acd58c3a70665a507e1eb665e839fc01c025012143fd5',
'/tmp/git-zero-fit-response-independent-review/REVIEW.json':'031f1b914fb282f0a355ff8cdd22fb708704297129f52ccf56207476bf8d4459',
'/tmp/git-zero-fit-response-independent-review-supplement-01/REVIEW.md':'ca496616b812171b11a9bb7956ecefb8fb289e86ef39f34e35e47e67dcec1f06',
'/tmp/git-zero-fit-response-independent-review-supplement-01/REVIEW.json':'8ed822f0c3681864117acbffc28f8fba8fe611d29d80a29248a6fa1c322e2585'}
for path,digest in prior.items():check(sha(path)==digest,'previous review changed',path=path)
for path in load('/tmp/git-zero-fit-response-source-ownership-v0.json')['stdlib_source']:record(path['path'])
for path in ['collections/btree/node.rs','../../core/src/result.rs']:
    record(pathlib.Path('/var/home/pawel/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src')/path)
for name in ['native.rs','cli.rs','runner.rs','manifest.json']:record(pathlib.Path('/tmp/git-zero-fit-response-final-source')/name)
record('/tmp/git-zero-fit-response-target-j2/debug/repository-host');record('/tmp/git-zero-fit-response-target-j2/debug/deps/multi_chunk_cloudflare-dcddae24be8f3d8c')
git=lambda *args:subprocess.run(['git','-c','core.pager=cat',*args],cwd=WT,check=True,capture_output=True).stdout
source={'head':git('rev-parse','HEAD').decode().strip(),'parent':git('rev-parse','HEAD^').decode().strip(),'status':git('status','--porcelain=v1').decode(),'Native_tree':git('rev-parse','HEAD:examples/repository-host/src').decode().strip(),'Worker_tree':git('rev-parse','HEAD:examples/repository-host/cloudflare/src').decode().strip(),'changed_from_approved_base':git('diff','--name-only','b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0','HEAD').decode().splitlines()}
final_patch=git('diff','HEAD^','HEAD');check(hashlib.sha256(final_patch).hexdigest()=='79919a8a6e4fb9c90132b3fda9de24b21ac2936e9e2fca98a39be22667455981','final panic-only patch SHA')
tested=(pathlib.Path('/tmp/git-zero-fit-response-build-v2/runner.rs')).read_bytes();final=(BASE/'tests/multi_chunk_cloudflare.rs').read_bytes();old=b'.err()\n                .expect("incomplete fault accounting must refuse a priced estimate")';new=b'.expect_err("incomplete fault accounting must refuse a priced estimate")'
check(tested.count(old)==1 and tested.replace(old,new)==final,'final exact source transformation')
check(git('show','8287ef863e8ec78950ed3a9a59d509a7ed6e1dc2:examples/repository-host/tests/multi_chunk_cloudflare.rs')==tested,'tested source equals Git object')
check(source['head']=='95fdf58f6bd7298736cfe124725e4f7e6c9aceb9'and source['parent']=='8287ef863e8ec78950ed3a9a59d509a7ed6e1dc2'and not source['status'],'final clean commit')
check(set(source['changed_from_approved_base'])=={'examples/repository-host/src/native.rs','examples/repository-host/src/bin/repository-host.rs','examples/repository-host/tests/multi_chunk_cloudflare.rs'},'final scope changed unexpectedly')
(OUT/'approved-base-to-final.diff').write_bytes(git('diff','b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0','HEAD'))
(OUT/'tested-v2-to-final.diff').write_bytes(final_patch)
summary={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'errors':errors,'source':source,'four_completed_controls':scope_results,'v2_source_maps_equal_across_scopes':all(v==v2maps[0]for v in v2maps),'fault_coverage':fault_result,'versioned_source_checks':source_checks,'runtime_snapshots':runtime,'runtime_snapshot_count':len(runtime),'distinct_Native_processes':len(native),'restart_native_roots':restart_native,'tested_to_final_exact_transformation':tested.replace(old,new)==final,'previous_reviews_unchanged':all(sha(p)==h for p,h in prior.items()),'observed_one_ulp_timing_advisory_cost_roundtrips':float_roundtrips,'receipt_comparison_rule':'Integer/string/bool/NULL identity is exact. Only separately enumerated one-ULP timing/advisory-cost float roundtrips accepted; raw bytes/hashes unchanged. No priced or speed approval.','source_approval_candidate':'SOURCEAPPROVED only if remaining actual cleanup/historical-gap audit agrees'}
(OUT/'completion-semantic-audit.json').write_text(json.dumps(summary,indent=2)+'\n')
inv['files']=sorted(records.values(),key=lambda r:r['path']);inv['audit']['input_records']=len(inv['files']);inv['audit']['semantic_audit_errors']=errors
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps(inv,indent=2)+'\n');(OUT/'INPUT_SHA256SUMS').write_text(''.join(r['sha256']+'  '+r['path']+'\n'for r in inv['files']))
print(json.dumps({'errors':errors,'inputs':len(inv['files']),'scopes':[{'scope':s['scope'],'rows':s['rows'],'bounds':s['bounds'],'observer':s['final_observer']}for s in scope_results],'fault_counts':fault_result['kinds'],'runtime_snapshots':len(runtime),'Native_processes':len(native),'exact_final_transformation':summary['tested_to_final_exact_transformation']},indent=2))
