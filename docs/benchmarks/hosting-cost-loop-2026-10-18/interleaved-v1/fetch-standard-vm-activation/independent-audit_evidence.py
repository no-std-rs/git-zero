from pathlib import Path
import hashlib,json,os,datetime,collections,stat,tarfile
OUT=Path('/tmp/git-zero-fetch-portability-independent-review')
HAND=Path('/tmp/git-zero-fetch-portability-handoff-20261018-01')
RAW=Path('/tmp/git-zero-fetch-portability-tiny-20261018-01')
CASE=RAW/'case';inputs={}
def digest(b):return hashlib.sha256(b).hexdigest()
def read(p):p=Path(p);b=p.read_bytes();inputs[str(p)]=digest(b);return b
def js(p):return json.loads(read(p))
def check(p,h):assert digest(read(p))==h,str(p)
manifest=js(HAND/'runtime-manifest.json')
rawinv=[json.loads(l) for l in read(HAND/'tiny-raw-inventory.jsonl').splitlines()]
assert len(rawinv)==671
for row in rawinv:
 p=Path(row['path']);assert p==RAW/row['relative'];b=read(p);assert len(b)==row['bytes'] and digest(b)==row['sha256'],str(p)
assert {str(p) for p in RAW.rglob('*') if p.is_file()}=={r['path'] for r in rawinv}
assert sum(r['bytes'] for r in rawinv)==1804531
proof=js(RAW/'proof.json');assert proof==js(HAND/'tiny-proof.json') and proof['pass']
check(RAW/'proof.json','120aff4fc293ae39aca57701e9040146330726900e602e93dab1e979ab1f77ab')
plan=js(RAW/'active-plan.json');assert read(RAW/'active-plan.json')==read('/tmp/git-zero-fetch-stage3-tiny-production-20261018-02/active-plan.json')
assert proof['plan_sha256']==digest(read(RAW/'active-plan.json'))
superv=proof['supervision'];assert superv==js(CASE/'supervision.json')
assert superv['elapsed_ms']==45552 and superv['deadline_ms']==120000 and superv['cleanup_grace_ms']==5000
for k in ['leader_reaped','parent_outside_case_group','pipe_joins_complete']:assert superv[k] is True,k
for k in ['deadline_expired','kill_sent','ownership_lost','pipe_drain_blocked_after_leader_exit']:assert superv[k] is False,k
assert superv['term_sent_at_ms'] is None and superv['leader_exit_code']==0 and superv['leader_signal'] is None
assert superv['remaining_owned_processes']==[] and superv['signals_and_errors']==[]
pgid=superv['owned_pgid'];assert pgid==3108055
commands=[]
for p in sorted(CASE.glob('cmd-*.json')):
 if p.name.endswith('.started.json'):continue
 row=js(p);stem=p.name[:-5];start=js(p.with_name(stem+'.started.json'))['process']
 assert row['success'] and row['exit_code']==0 and row['signal'] is None and row['streams_complete']
 assert row['pgid']==pgid==start['pgid'] and row['pid']==start['pid']
 for stream in ['stdout','stderr']:
  b=read(p.with_name(stem+'.'+stream));assert len(b)==row[stream+'_bytes']==row[stream+'_prefix_bytes']
  assert len(b)<=131072 and digest(b)==row[stream+'_sha256']
 commands.append((p,row))
assert len(commands)==104
assert read(CASE/'cmd-0002-actual-git-version.stdout')==b'git version 2.53.0\n'
assert read(CASE/'cmd-0003-actual-node-version.stdout')==b'v24.10.0\n'
assert read(CASE/'cmd-0004-actual-wrangler-version.stdout').strip().endswith(b'4.149.0')
# Independent tiny Git-object construction uses memory only; no Git/object/fixture writes.
MASK=(1<<64)-1
def content(idx,n):
 s=0x9e3779b97f4a7c15^(idx+1);out=bytearray();prefix=b'fn item() { return 42; }\n'
 for i in range(n):
  s^=(s<<13)&MASK;s^=s>>7;s^=(s<<17)&MASK;s&=MASK
  out.append(prefix[i%25] if i%1024<512 else s&255)
 return out
def obj(kind,b):return hashlib.sha1(kind.encode()+b' '+str(len(b)).encode()+b'\0'+b).digest()
files={f'{i}.dat':content(i,4096) for i in range(4)};files['large.dat']=content(10001,16384)
expected=[];parent=None
for g in range(1,4):
 if g>1:
  change=f'interleaved generation {g:02}\n'.encode();files[f'{g%4}.dat'][2048:2048+len(change)]=change
 subtree=b''.join(b'100644 '+n.encode()+b'\0'+obj('blob',b) for n,b in sorted(files.items()))
 root=b'100644 .gitattributes\0'+obj('blob',b'data/large.dat -delta\n')+b'40000 data\0'+obj('tree',subtree)
 tree=obj('tree',root).hex();body=f'tree {tree}\n'
 if parent:body+=f'parent {parent}\n'
 body+=f'author Benchmark <benchmark@example.invalid> 1767225600 +0000\ncommitter Benchmark <benchmark@example.invalid> 1767225600 +0000\n\nInterleaved generation {g:02}\n'
 tip=obj('commit',body.encode()).hex();expected.append({'generation':g,'tip':tip,'tree':tree});parent=tip
assert expected==plan['logical_fixture']['generations']
for n,b in files.items():assert read(CASE/'source/data'/n)==b
assert read(CASE/'source/.gitattributes')==b'data/large.dat -delta\n'
verify=js(CASE/'fixture-verification.json');assert verify['all_expected_verified'] and verify['verified_before_runtime_start']
for got,e in zip(verify['verified_generations'],expected):assert got['tip']==e['tip']==got['expected_tip'] and got['tree']==e['tree']==got['expected_tree'] and got['pass']
ops=[json.loads(l) for l in read(CASE/'operations.jsonl').splitlines()]
assert len(ops)==18
verified=[]
for i,op in enumerate(plan['schedule_prefix_including_target']):
 start,done=ops[2*i:2*i+2];assert start['kind']=='operation_started' and done['kind']=='operation_verified'
 row=done['row'];assert start['operation']==op==row['operation'] and row['complete']
 assert start['after'] is None and start['complete'] is False
 for name in ['before','after','post_check']:assert row[name]==js(CASE/(op['key']+'-'+('post-verify' if name=='post_check' else name)+'.body'))
 verified.append(row)
fetches=[r for r in verified if r['operation']['kind']=='fetch'];assert len(fetches)==6
bylabel=collections.defaultdict(list)
for p,row in commands:bylabel[row['label']].append((p,row))
strict=[]
for row in fetches:
 op=row['operation'];want=op['want'];want_tree=next(e['tree'] for e in expected if e['tip']==want)
 assert row['verified_want']==want and row['verified_tree']==want_tree and row['strict_fsck']
 assert row['new_packs']
 for pack in row['new_packs']:
  receipt=js(pack['strict_index_pack']['receipt']);assert receipt['success'] and receipt['complete'] and receipt['error'] is None
  assert receipt['pack_sha256']==pack['sha256'] and receipt['pack_bytes']==pack['bytes']>0
  assert receipt['git_arguments']==['index-pack','--strict',receipt['pack_path']]
  _,cmd=bylabel[receipt['command_label']][0];assert pack['strict_index_pack']['stdout_sha256']==cmd['stdout_sha256']
  p=Path(receipt['pack_path']);retained=p.exists()
  if retained:check(p,pack['sha256']);assert p.stat().st_size==pack['bytes']
  strict.append({'key':op['key'],'pack_sha256':pack['sha256'],'bytes':pack['bytes'],'retained_pack_rehashed':retained})
assert len(strict)==6
assert len(bylabel['strict-fsck'])==len(bylabel['verify-want'])==len(bylabel['verify-tree'])==len(bylabel['source-tree'])==6
for (_,a),(_,b) in zip(bylabel['archive'],bylabel['source-archive']):assert a['stdout_bytes']==b['stdout_bytes'] and a['stdout_sha256']==b['stdout_sha256']
# Independently check retained pack framing/trailer, without invoking Git.
packfiles=list(CASE.rglob('*.pack'))
for p in packfiles:
 b=read(p);assert b[:4]==b'PACK' and int.from_bytes(b[4:8],'big') in [2,3] and hashlib.sha1(b[:-20]).digest()==b[-20:]
http=list(CASE.glob('*.http.json'));assert len(http)==30
for p in http:
 row=js(p);b=read(p.with_name(p.name.removesuffix('.http.json')+'.body'))
 assert row['complete'] and row['error'] is None and row['status']==200 and len(b)==row['body_bytes'] and digest(b)==row['body_sha256']
resource=[p for p in CASE.glob('resource-boundary-*.json') if not p.name.endswith('.started.json')];assert len(resource)==15
for p in resource:
 row=js(p);started=js(p.with_name(p.name.removesuffix('.json')+'.started.json'))
 assert row['complete'] and row['capacity_pass'] and started['case_pgid']==pgid
 assert row['aggregate_prefix']==str(RAW) and row['aggregate_logical_bytes']<=16777216
 assert row['available_memory_bytes']>=row['minimum_memory_bytes']==134217728
 assert row['available_disk_bytes']>=row['disk_reserve_bytes']+16777216 and row['disk_reserve_bytes']==2147483648
before=js(CASE/'source-runtime-before.json');after=js(CASE/'source-runtime-after.json');assert before==after
assert before['runtime_manifest']==manifest and before['runtime_manifest_sha256']==digest(read(HAND/'runtime-manifest.json'))
for row in before['source_files']:check(row['path'],row['sha256'])
for name in ['runtime-live-before.json','runtime-live-after.json']:
 live=js(CASE/name);assert live['all_descendants_inherit'] and live['case_pgid']==pgid
 for row in live['processes']:
  assert row['pgid']==pgid
  if row['exe'] in [e['path'] for e in manifest['binaries'].values()]:assert row['exe_sha256']==next(e['sha256'] for e in manifest['binaries'].values() if e['path']==row['exe'])
env=js(CASE/'native-environment.json');assert env==proof['result']['native_environment']==js(HAND/'native-environment.json')
ready=js(CASE/'native-readiness.json');native=js(CASE/'native-process.json')
assert int(env['capture_begin_unix_ns'])-int(ready['observed_unix_ns'])==108604
assert int(env['capture_end_unix_ns'])-int(env['capture_begin_unix_ns'])==1938872323
assert env['raw_environment_bytes']==5438 and env['raw_environment_sha256']=='2d58c689539c44b244810d54ed4d1a8e0a59d7d608cb4f12fdfe35d61c181bcb'
assert env['read_after_existing_readiness_log'] and ready['added_service_probe'] is False
assert env['identity_environment_eligible'] and env['raw_read_complete'] and env['pid_start_pgid_and_elf_match']
assert env['raw_read_error_kind'] is None and not env['raw_environment_retained'] and not env['duplicate_git_names']
assert env['invalid_git_names']==[] and env['missing_canonical_git_names']==[]
assert env['safe_git_values']=={'GIT_CONFIG_COUNT':'0','GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1','GIT_TERMINAL_PROMPT':'0','GIT_ZERO_PHASE_LEDGER':str(CASE/'native-phase.jsonl')}
assert env['git_environment_names']==sorted(env['safe_git_values'])
for phase in ['before','after']:
 r=env['process_'+phase];assert r['pid']==native['pid']==ready['pid']==3108522 and r['start_ticks']==native['start_ticks']==ready['start_ticks']==57763925 and r['pgid']==pgid
 assert env['actual_exe_sha256_'+phase]==manifest['binaries']['native']['sha256']
# Full retained phase/request projection and exact paid-counter arithmetic.
ledger=[json.loads(l) for l in read(CASE/'native-phase.jsonl').splitlines()];assert len(ledger)==127 and len({r['record'] for r in ledger})==127
assert all(r['outcome']=='complete' and r['pid']==native['pid'] and r['process_start_ns']<=r['process_end_ns'] for r in ledger)
requests=js(CASE/'actual-request-hashes.json')
projection=[{k:r[k] for k in ['request','kind','fields','outcome']} for r in ledger if r['kind'] in ['http_request','native_read']]
assert requests==projection and len(requests)==61
closure=js(CASE/'scope-closure.json');total=closure['all_actual_counters'];usage=js(CASE/'final-service-usage.body');assert closure['complete']
counters=[r['counter'] for r in closure['phase_checks']];assert len(counters)==12
for key in counters:
 assert total[key]==usage[key]==sum(v[key] for v in total['by_phase'].values())
 assert closure['observer_and_cleanup_stats'][key]==total['by_phase']['stats'][key]
 assert closure['ordinary_workload_including_startup_excluding_stats'][key]==total[key]-total['by_phase']['stats'][key]
 assert closure['startup'][key]==total['by_phase']['startup'][key]==verified[0]['before']['cloudflare_services']['by_phase']['startup'][key]
for row in verified:
 for key in counters:
  a=row['before']['cloudflare_services'];b=row['after']['cloudflare_services']
  def paid(v):return v[key]-v['by_phase'].get('stats',{}).get(key,0)
  assert paid(b)-paid(a)==row['workload_delta'][key]
metrics=proof['result']['final_native_metrics'];float_roundtrips=[]
def equal_json(a,b,path=''):
 if isinstance(a,dict):
  assert isinstance(b,dict) and a.keys()==b.keys(),path
  for k in a:equal_json(a[k],b[k],path+'/'+k)
 elif isinstance(a,list):
  assert isinstance(b,list) and len(a)==len(b),path
  for i,(x,y) in enumerate(zip(a,b)):equal_json(x,y,path+'/'+str(i))
 elif a!=b:
  assert isinstance(a,float) and isinstance(b,float) and abs(a-b)<=1e-12,(path,a,b)
  float_roundtrips.append({'path':path,'absolute_difference':abs(a-b)})
equal_json(metrics,js(CASE/'final-object-stats.body'))
metric_keys=['upload_advertisement_certificates','upload_advertisement_shortcuts','generation_witness_hits','gateway_read_consumed','gateway_read_plans','gateway_receive_consumed','gateway_receive_plans','history_projections','history_lineage_bytes']
for name in ['native','wrangler']:
 cleanup=js(CASE/(name+'-ordinary-cleanup.json'));assert cleanup['leader_reaped']
 for stream,r in zip(['stdout','stderr'],cleanup['streams']):
  b=read(CASE/(name+'.'+stream));assert r['complete'] and len(b)==r['bytes']==r['prefix_bytes'] and digest(b)==r['sha256']
assert js(CASE/'native-ordinary-cleanup.json')['signal']==15
# Read only numeric process metadata and TCP tables; do not read commandlines/environments.
pairs=set()
def collect(v):
 if isinstance(v,dict):
  if isinstance(v.get('pid'),int) and isinstance(v.get('start_ticks'),int):pairs.add((v['pid'],v['start_ticks']))
  for x in v.values():collect(x)
 elif isinstance(v,list):
  for x in v:collect(x)
for p in RAW.rglob('*.json'):collect(js(p))
assert len(pairs)==119,len(pairs)
remaining=[];groups=[]
for p in Path('/proc').iterdir():
 if not p.name.isdigit():continue
 try:
  raw=(p/'stat').read_text();rest=raw[raw.rfind(')')+2:].split();pid=int(p.name);start=int(rest[19]);group=int(rest[2])
 except (OSError,ValueError,IndexError):continue
 if (pid,start) in pairs:remaining.append({'pid':pid,'start_ticks':start,'pgid':group})
 if group==pgid:groups.append({'pid':pid,'start_ticks':start,'pgid':group})
listeners=[]
for table in ['/proc/net/tcp','/proc/net/tcp6']:
 for line in Path(table).read_text().splitlines()[1:]:
  fields=line.split();port=int(fields[1].rsplit(':',1)[1],16)
  if fields[3]=='0A' and port in [8853,8854]:listeners.append({'table':table,'port':port,'local':fields[1]})
assert remaining==[] and groups==[] and listeners==[]
cleanup={'time_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'exact_recorded_pid_start_pairs_checked':len(pairs),'owned_pgids':[pgid],'remaining_exact_owned_generations':remaining,'remaining_owned_group_members':groups,'own_port_listeners':listeners,'pass':True,'signals_sent':False,'other_owner_environment_or_cmdline_read':False}
(OUT/'cleanup-readonly.json').write_text(json.dumps(cleanup,indent=2)+'\n')
result={'pass':True,'proof_sha256':digest(read(RAW/'proof.json')),'raw_files_rehashed':len(rawinv),'raw_logical_bytes':sum(r['bytes'] for r in rawinv),'same_previous_tiny_plan':True,'independently_recreated_tiny_ids':expected,'working_tree_bytes':sum(len(b) for b in files.values())+len(b'data/large.dat -delta\n'),'operations':len(verified),'pushes':3,'reads':6,'successful_commands':len(commands),'fully_rehashed_command_streams':len(commands)*2,'strict_index_checks':strict,'retained_pack_framing_and_trailers':len(packfiles),'http_status_body_receipts':len(http),'supervised_resource_boundaries':len(resource),'source_runtime_before_after_equal':True,'all_source_runtime_references_currently_rehashed':True,'native_environment_qualified':True,'raw_environment_not_retained_or_rehashed_by_reviewer':True,'raw_environment_bytes_receipt':env['raw_environment_bytes'],'raw_environment_sha256_receipt':env['raw_environment_sha256'],'capture_after_ready_ns':108604,'capture_duration_ns':1938872323,'native_pid':native['pid'],'native_start_ticks':native['start_ticks'],'owned_pgid':pgid,'phase_records':len(ledger),'request_hash_records':len(requests),'boot_anchor_uncertainty_ns':max(r['boot_anchor_uncertainty_ns'] for r in ledger),'counter_identities':len(counters),'float_roundtrip_differences':float_roundtrips,'float_tolerance':1e-12,'per_operation_integer_counter_deltas':len(verified)*len(counters),'applicable_metrics':{k:metrics[k] for k in metric_keys},'supervisor_elapsed_ms':superv['elapsed_ms'],'outer_timeout_or_signals':False,'ordinary_service_cleanup_complete':True,'cpu_ms':None,'do_gb_seconds':None,'full_bill_usd':None,'performance_claim':False,'cleanup':cleanup}
(OUT/'tiny-evidence.json').write_text(json.dumps(result,indent=2)+'\n')
(OUT/'evidence-inputs.json').write_text(json.dumps(inputs,indent=2)+'\n')
print(json.dumps({k:result[k] for k in ['pass','raw_files_rehashed','raw_logical_bytes','operations','successful_commands','fully_rehashed_command_streams','retained_pack_framing_and_trailers','http_status_body_receipts','supervised_resource_boundaries','phase_records','request_hash_records','counter_identities','per_operation_integer_counter_deltas','supervisor_elapsed_ms']},indent=2))
