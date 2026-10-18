import pathlib,json,hashlib,datetime,subprocess
OUT=pathlib.Path('/tmp/git-zero-fit-response-independent-review')
ROOT=pathlib.Path('/tmp/git-zero-fit-response-correctness-manifests')
WT=pathlib.Path('/tmp/git-zero-hosting-fit-response')
BASE=WT/'examples/repository-host'
raw=json.load(open('/tmp/git-zero-fit-response-raw-evidence-inventory.json'))
rawmap={v['relative_path']:v for v in raw['files']}
inputs=json.load(open(OUT/'INPUT_SHA256_INVENTORY.json'))
actual={v['path']:v for v in inputs['files']}
def record(p):
    p=str(p)
    if p not in actual:
        h=hashlib.sha256()
        with open(p,'rb') as f:
            while b:=f.read(1024*1024):h.update(b)
        s=pathlib.Path(p).stat();actual[p]={'path':p,'bytes':s.st_size,'mtime_ns':s.st_mtime_ns,'allocated_bytes':s.st_blocks*512,'sha256':h.hexdigest()}
    return actual[p]
errors=[]
scope_provenance=[]
for d in sorted(ROOT.iterdir()):
    if not d.is_dir():continue
    before=json.load(open(d/'source-before.json'));after=json.load(open(d/'source-after.json'))
    version='v0' if d.name.endswith('-v0') else 'v1'
    mismatches=[]
    for name,v in before.items():
        if name=='tests/multi_chunk_cloudflare.rs':p=pathlib.Path('/tmp/git-zero-fit-response-build-'+version)/'runner.rs'
        elif name=='runner_binary':p=pathlib.Path('/tmp/git-zero-fit-response-build-'+version)/'runner-elf'
        elif 'path' in v:p=pathlib.Path(v['path'])
        else:p=BASE/name
        r=record(p)
        if any(r[k]!=v[k] for k in ['bytes','sha256']):mismatches.append({'name':name,'path':str(p)})
    runtime=[]
    for p in sorted(d.glob('runtime-*.json')):
        entries=json.load(open(p));procs=[]
        for pid,v in entries.items():
            exe=pathlib.Path(v['exe']);kind='native' if exe.name=='repository-host' else 'node' if exe.name=='node' else 'workerd' if exe.name=='workerd' else 'ancillary'
            expected=before.get({'native':'native_binary','node':'runtime_node','workerd':'runtime_workerd_binary'}.get(kind,''))
            digest_ok=expected is None or expected['sha256']==v['elf_sha256']
            environment=v.get('actual_git_environment',{})
            clean_env='GIT_EDITOR' not in environment and 'GIT_TRACE2_EVENT' not in environment if kind=='native' else None
            map_lines=[line for line in v['maps'].splitlines() if line.rstrip().endswith(str(exe)) or line.rstrip().endswith(str(exe)+' (deleted)')]
            procs.append({'pid':pid,'kind':kind,'exe':str(exe),'elf_sha256':v['elf_sha256'],'matches_expected_elf':digest_ok,'native_git_editor_trace2_absent':clean_env,'executable_map_lines':len(map_lines)})
            if not digest_ok or clean_env is False or not map_lines:errors.append({'scope':d.name,'snapshot':p.name,'process':pid,'reason':'runtime provenance'})
        runtime.append({'snapshot':p.name,'processes':procs})
    if before!=after or mismatches:errors.append({'scope':d.name,'source_mismatches':mismatches,'before_after_equal':before==after})
    scope_provenance.append({'scope':d.name,'version':version,'source_before_after_equal':before==after,'source_map_files_verified':len(before),'source_actual_mismatches':mismatches,'runtime_snapshots':runtime})
def rowid(row):return row.get('name',row.get('kind'))
def delta(row,key):return row['after'][key]-row['before'][key]
def wire(scope,row):
    name=row.get('name')
    if name is None:
        candidates=[v for rel,v in rawmap.items() if rel.startswith(scope+'/') and v['sha256']==row.get('response_sha256')]
        return {'row':rowid(row),'sha256':row.get('response_sha256'),'matching_raw_files':[v['relative_path'] for v in candidates]}
    key=scope+'/'+name+'.body';v=rawmap.get(key)
    okay=v is not None and v['sha256']==row['response_sha256'] and v['bytes']==row['response_bytes']
    if not okay:errors.append({'scope':scope,'row':name,'reason':'raw wire mismatch'})
    return {'row':name,'raw_body_sha_matches':okay,'body_bytes':row['response_bytes']}
pairs=[]
known=['r2_class_a','r2_deletes','r2_written_bytes','r2_live_bytes','r2_live_objects','r2_peak_bytes','do_sqlite_bytes']
for size in [16,64]:
    es=f'first{size}-evict-v0';fs=f'first{size}-fit-v0';e=json.load(open(ROOT/es/'contract.json'));f=json.load(open(ROOT/fs/'contract.json'))
    compared=[]
    for er,fr in zip(e['rows'],f['rows']):
        eq=er.get('response_sha256')==fr.get('response_sha256') if 'response_sha256' in er and 'response_sha256' in fr else None
        compared.append({'row':rowid(er),'same_row':rowid(er)==rowid(fr),'measured_wire_equal':eq,'evict_wire':wire(es,er) if 'response_sha256'in er else None,'fit_wire':wire(fs,fr) if 'response_sha256'in fr else None,'evict_pressure_delta':delta(er,'protected_chunk_pressure_evictions'),'fit_pressure_delta':delta(fr,'protected_chunk_pressure_evictions'),'fit_skip_delta':delta(fr,'upload_response_retention_skips'),'fit_hit_delta':delta(fr,'protected_chunk_hits'),'fit_hit_bytes_delta':delta(fr,'protected_chunk_hit_bytes')})
    ek=e['final']['cloudflare_services'];fk=f['final']['cloudflare_services']
    counts={p:{'startup_paid_GET':c['startup_paid_recovery']['cloudflare_services']['r2_class_b'],'final_GET':c['final']['cloudflare_services']['r2_class_b'],'post_startup_GET':c['final']['cloudflare_services']['r2_class_b']-c['startup_paid_recovery']['cloudflare_services']['r2_class_b'],'pressure_evictions':c['final']['protected_chunk_pressure_evictions'],'retention_skips':c['final']['upload_response_retention_skips']} for p,c in [('evict',e),('fit',f)]}
    pairs.append({'initial_mib':size,'rows_per_arm':len(e['rows']),'fixture_seals_equal':json.load(open(ROOT/es/'fixture-input-seal.json'))==json.load(open(ROOT/fs/'fixture-input-seal.json')),'input_manifest_equal':e['input_manifest']==f['input_manifest'],'final_manifest_equal':e['final_manifest']==f['final_manifest'],'physical_objects_equal':e['final_object_stats']['objects']==f['final_object_stats']['objects'],'measured_wire_matches':sum(x['measured_wire_equal'] is True for x in compared),'unmeasured_rows':[x['row'] for x in compared if x['measured_wire_equal'] is None],'counts':counts,'known_storage':{k:{'evict':ek[k],'fit':fk[k],'equal':ek[k]==fk[k]} for k in known},'r2_peak_objects':{'evict':None,'fit':None,'equal':None,'status':'NOT_METERED'},'known_ops':{op:{'evict':ek['operations'][op],'fit':fk['operations'][op],'equal':ek['operations'][op]==fk['operations'][op]} for op in ['r2_put','r2_delete','r2_list','manifest_cas']},'rows':compared})
n=json.load(open(ROOT/'near-limit-fit-v1'/'contract.json'))
near={'rows':{k:{'response_bytes':n[k]['response_bytes'],'gets':n[k]['gets'],'cache_hit_delta':delta(n[k],'cache_hits'),'retention_admission_delta':delta(n[k],'upload_response_retention_admissions'),'cache_entries_after':n[k]['after']['cache_entries'],'verified_chunks_after':n[k]['after']['protected_chunk_entries'],'wire':wire('near-limit-fit-v1',n[k])} for k in ['first','repeat','over','over_repeat']},'fitting_identical_request':rawmap['near-limit-fit-v1/near-limit-first.request']['sha256']==rawmap['near-limit-fit-v1/near-limit-repeat.request']['sha256'],'oversize_identical_request':rawmap['near-limit-fit-v1/over-limit-first.request']['sha256']==rawmap['near-limit-fit-v1/over-limit-repeat.request']['sha256']}
p=json.load(open(ROOT/'plan-fit-v1'/'plan-fill-contract.json'))
held={'paid_discard_delta':p['after']['gateway_read_paid_discards']-p['before']['gateway_read_paid_discards'],'verified_admission_delta':p['after']['protected_chunk_admissions']-p['before']['protected_chunk_admissions'],'prune_delta':p['after']['protected_chunk_prunes']-p['before']['protected_chunk_prunes'],'gateway_hook_fired':p['trace']['gateway_hook_fired'],'held_first_chunks':p['trace']['gateway_plan']['plan']['first'],'wire':wire('plan-fit-v1',p['response']),'test_wrapper':'unchanged socket-rpc-worker.js -> first-read-worker.js; continue-cache-fill issues actual ordinary Native upload, then held authenticated continuation'}
review={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'errors':errors,'scope_provenance':scope_provenance,'pairs':pairs,'near_limit':near,'held_mask':held}
(OUT/'semantic-provenance-audit.json').write_text(json.dumps(review,indent=2)+'\n')
inputs['files']=sorted(actual.values(),key=lambda x:x['path']);inputs['audit']['recorded_inputs']=len(inputs['files']);inputs['audit']['supplement']='Includes current source files and external runtime source-map files, verified against v0/v1 preserved sources and ELFs; raw bytes previously audited completely.'
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps(inputs,indent=2)+'\n')
print(json.dumps({'errors':errors,'verified_scopes':len(scope_provenance),'inputs':len(inputs['files']),'pairs':[{'mib':p['initial_mib'],'rows':p['rows_per_arm'],'wire_matches':p['measured_wire_matches'],'counts':p['counts']} for p in pairs],'near':near,'held':held},indent=2))
