#!/usr/bin/env python3
"""Independent offline arithmetic only. No native/workerd/benchmark/build calls.
Written from immutable R19 source semantics and original fresh reports; does not
read or execute the auditor's embedded verification_program.
"""
import json,hashlib,subprocess,datetime,pathlib,collections,math
BASE=pathlib.Path('/tmp/git-zero-fit-response-independent-design-review')
ROOT='/tmp/git-zero-hosting-cost-loop'; COMMIT='f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5'
AUD=pathlib.Path('/tmp/git-zero-after-joint-residual-audit.json')
audit=json.loads(AUD.read_text()); MiB=1048576; BUDGET=32*MiB; ENTRY=16*MiB

def sha(b):return hashlib.sha256(b).hexdigest()
def seal(p):
 b=pathlib.Path(p).read_bytes();return dict(path=str(p),bytes=len(b),sha256=sha(b))
def whole(v):
 assert v==int(v);return int(v)
def original_rows(report,sample):
 rows=[r for r in report['raw'] if r.get('backend')=='cloudflare-local' and r.get('sample')==sample and r.get('operation')]
 assert len(rows)==88 and [r['operation'] for r in rows]==report['fixture']['schedule'];return rows

# Actual advertisement encoding: learned capabilities weight = len + 256;
# packet overheads come from advertisement.rs, not certificate weight as body.
def advert_body(g,report):
 caps=whole(next(r for r in original_rows(report,0) if r['operation']['key']=='latest_fetch_01')['service_metrics_delta']['upload_advertisement_template_bytes'])-256
 if caps <= 0: # warm path does not learn a certificate; same frozen Git identity.
  caps=222
 refs=['refs/heads/main']+[f'refs/heads/history-{i}' for i in range(1,g+1)]
 return (4+len(b'# service=git-upload-pack\n')+4 + 4+40+len(b' HEAD\0')+caps+1
         +sum(4+40+1+len(n)+1 for n in refs)+4)
# Native 64-bit exact cloned response weights: 64 key + 16 Arc +
# (12 name + content-type len + 12*48 BTree + 16*8) + 256.
def overhead(content_type):return 64+16+12+len(content_type)+12*48+16*8+256
DISC_O=overhead('application/x-git-upload-pack-advertisement') # 1095
POST_O=overhead('application/x-git-upload-pack-result') # 1088
assert (DISC_O,POST_O)==(1095,1088)

class Model:
 def __init__(self,cold,policy,stress):
  self.cold=cold;self.policy=policy;self.stress=stress;self.packs=[];self.owners=[];self.capacity=0;self.responses=[];self.stats=collections.Counter();self.rows=[];self.skips=[];self.inventories=[]
 def chunkweight(self):return sum(n+399 for _,n in self.owners)+8*self.capacity+(256 if self.capacity else 0)
 def responseweight(self):return sum(w for _,w in self.responses)
 def charge(self):return self.chunkweight()+self.responseweight()+self.stress
 def evict(self):
  if self.owners:
   idx=(1 if len(self.owners)>1 else 0) if self.policy=='oldest_secondary' else 0 if self.policy=='oldest_all' else max(range(len(self.owners)),key=lambda i:self.owners[i][1]) if self.policy=='largest' else -1
   self.owners.pop(idx);self.stats['pressure']+=1
   if not self.owners:self.capacity=0
  else:
   assert self.responses;self.responses.pop(0);self.stats['response_evictions']+=1
 def trim(self,incoming,entry=False):
  while self.charge()+incoming>BUDGET or len(self.owners)+len(self.responses)+entry>256:self.evict()
 def admit(self,desc):
  if not self.cold or desc in self.owners:return
  if len(self.owners)>=32 or len(self.owners)+len(self.responses)>=256:self.stats['rejections']+=1;return
  growth=8*(len(self.owners)+1)+256 if len(self.owners)==self.capacity else 0
  needed=self.chunkweight()+desc[1]+399+growth
  if needed+self.responseweight()+self.stress>BUDGET or (self.policy=='r19' and self.owners and needed>17*MiB):self.stats['rejections']+=1;return
  if growth:self.capacity=len(self.owners)+1
  self.owners.append(desc);self.stats['admissions']+=1
 def read(self,selected):
  if not self.cold:return
  old=set(self.owners)
  for p in selected:
   for desc in p['chunks']:
    if desc in old:self.stats['hits']+=1;self.stats['hit_bytes']+=desc[1]
    else:self.stats['gets']+=1;self.stats['bytes']+=desc[1];self.admit(desc)
 def insert(self,key,body,weight,op):
  if body<1 or body>ENTRY:return
  if weight+self.stress>BUDGET:return
  if self.policy=='fit' and key!='discovery' and self.owners and (self.charge()+weight>BUDGET or len(self.owners)+len(self.responses)+1>256):
   self.skips.append(dict(operation=op,weight=weight,chunk_owners=len(self.owners),chunk_owning_bytes=self.chunkweight(),response_owning_bytes=self.responseweight()));self.stats['skips']+=1;return
  self.trim(weight,True);self.responses.append((key,weight))
 def step(self,row,report):
  op=row['operation'];key=op['key'];g=op['generation'];before=self.stats.copy();before_own=self.chunkweight();before_entries=len(self.owners);before_response=self.responseweight();before_respentries=len(self.responses)
  self.trim(0)
  selected=[]
  if op['kind']=='push':
   selected=list(self.packs);self.read(selected)
   n=whole(row['cloudflare_workload_delta']['r2_written_bytes'])
   # Actual production compaction selects the four smallest level-zero packs.
   # Here at every replacement exactly the previous three small appends and
   # new replacement are selected; source's 8 MiB job excludes initial pack.
   removed=[];floor=g;level=0
   if whole(row['compaction_delta']['count']):
    removed=sorted([p for p in self.packs if p['level']==0],key=lambda p:(p['bytes'],p['serial']))[:3]
    assert len(removed)==3
    floor=min(p['floor'] for p in removed);level=1
    self.packs=[p for p in self.packs if p not in removed]
   chunks=[(f'{g}/{i}',min(ENTRY,n-i*ENTRY)) for i in range(math.ceil(n/ENTRY))]
   self.packs.append(dict(serial=g,floor=floor,level=level,bytes=n,chunks=chunks))
   live={d for p in self.packs for d in p['chunks']}
   prior=len(self.owners);self.owners=[d for d in self.owners if d in live];self.stats['prunes']+=prior-len(self.owners)
   if not self.owners:self.capacity=0
   self.responses=[]
   inv=row['storage_after_push']['authoritative_services'];assert inv['objects']==len(live) and inv['logical_bytes']==sum(n for _,n in live) and inv['generation']==g
   self.inventories.append(dict(operation=key,chunks=sorted(live),ordered_pack_lengths=[p['bytes'] for p in self.packs]))
  else:
   body=advert_body(g,report)
   if not any(k=='discovery' for k,_ in self.responses):
    if g==1 and not self.responses:self.read(self.packs)
    self.insert('discovery',body,body+DISC_O,key)
   logical=(op['want'],op['have'])
   hit=any(k==logical for k,_ in self.responses)
   if hit:self.stats['response_hits']+=1
   else:
    cutoff=max(z['generation'] for z in report['fixture']['generations'] if z['tip'] in (op['want'],op['have']))
    selected=[p for p in self.packs if p['floor']<=cutoff]
    self.read(selected)
    post=whole(row['http_response_body_bytes'])-body
    self.insert(logical,post,post+POST_O,key)
  delta=self.stats-before
  rec=dict(operation=key,gets=delta['gets'],read_bytes=delta['bytes'],hits=delta['hits'],hit_bytes=delta['hit_bytes'],admissions=delta['admissions'],prunes=delta['prunes'],pressure=delta['pressure'],owning_bytes=self.chunkweight(),entries=len(self.owners),capacity=self.capacity,response_bytes=self.responseweight(),response_entries=len(self.responses),resident=[list(d) for d in self.owners],selected=[d for p in selected for d in p['chunks']])
  self.rows.append(rec)
  return rec

def run(report,sample,policy,stress):
 cold=report['config']['cloudflare_local']['view_cache']=='cold';m=Model(cold,policy,stress)
 for r in original_rows(report,sample):m.step(r,report)
 return m

inputs=[seal(AUD),seal('/tmp/git-zero-after-joint-residual-audit.md')]
sources=[]
for pinned in audit['source_blobs']:
 b=subprocess.check_output(['git','show',COMMIT+':'+pinned['path']],cwd=ROOT)
 actual=dict(path=pinned['path'],commit=COMMIT,bytes=len(b),sha256=sha(b));assert actual==pinned;sources.append(actual)
results=[];conditional={};negative=[];controls_rows=[];traces={}
for src in audit['inputs']:
 actual=seal(src['path']);assert actual==src;inputs.append(actual)
for p in audit['inputs'][-4:]:
 report=json.load(open(p['path']));profile=pathlib.Path(p['path']).stem
 for sample in range(2):
  original=original_rows(report,sample);current=run(report,sample,'r19',MiB)
  final_stats=next(x for x in report['cloudflare_runs'] if x['sample']==sample)['final_object_stats']
  initial_retained_bytes=whole(final_stats['cache_retained_bytes']-sum(r['service_metrics_delta']['cache_retained_bytes'] for r in original))
  cumulative=collections.Counter();boundary=[];errs=[]
  for raw,rec in zip(original,current.rows):
   for k,v in raw['service_metrics_delta'].items():
    if isinstance(v,(int,float)):cumulative[k]+=whole(v) if k not in ('upload_codec_seconds','receive_codec_seconds','receive_prepare_seconds','receive_publication_seconds','receive_storage_seconds','inline_maintenance_seconds','upload_advertisement_certification_seconds') else v
   expected=dict(gets=whole(raw['cloudflare_workload_delta']['r2_class_b']),read_bytes=whole(raw['cloudflare_workload_delta']['r2_read_bytes']),hits=whole(raw['service_metrics_delta']['protected_chunk_hits']),hit_bytes=whole(raw['service_metrics_delta']['protected_chunk_hit_bytes']),admissions=whole(raw['service_metrics_delta']['protected_chunk_admissions']),prunes=whole(raw['service_metrics_delta']['protected_chunk_prunes']),pressure=whole(raw['service_metrics_delta']['protected_chunk_pressure_evictions']),owning_bytes=whole(cumulative['protected_chunk_owning_bytes']),entries=whole(cumulative['protected_chunk_entries']),response_bytes=whole(cumulative['cache_bytes']),response_entries=whole(cumulative['cache_entries']))
   for k,v in expected.items():
    if rec[k]!=v:errs.append(dict(operation=rec['operation'],field=k,actual=v,model=rec[k]))
   boundary.append(initial_retained_bytes+whole(cumulative['cache_retained_bytes'])-rec['owning_bytes']-rec['response_bytes'])
  finalpacks=next(x for x in report['cloudflare_runs'] if x['sample']==sample)['final_object_stats']['manifest']['packs']
  finallengths=[sum(c['len'] for c in p['bundle']) for p in finalpacks]
  assert finallengths==[p['bytes'] for p in current.packs]
  results.append(dict(profile=profile,sample=sample,rows=88,push_inventories=len(current.inventories),all_counter_owning_checks_match=not errs,mismatches=errs,stats=dict(current.stats),initial_retained_bytes=initial_retained_bytes,boundary_control_min=min(boundary),boundary_control_max=max(boundary),final_ordered_pack_lengths=finallengths,readset_rows_sha256=sha(json.dumps(current.rows,sort_keys=True).encode())))
  traces[f'{profile}/{sample}']=dict(control=current.rows,inventories=current.inventories)
  for stress in (MiB//2,MiB,2*MiB,4*MiB):
   ctl=run(report,sample,'joint',stress);fit=run(report,sample,'fit',stress)
   regressions=[dict(operation=a['operation'],joint_gets=a['gets'],fit_gets=b['gets'],joint_bytes=a['read_bytes'],fit_bytes=b['read_bytes']) for a,b in zip(ctl.rows,fit.rows) if b['gets']>a['gets'] or b['read_bytes']>a['read_bytes']]
   no_superset=[a['operation'] for a,b in zip(ctl.rows,fit.rows) if not set(map(tuple,a['resident'])).issubset(set(map(tuple,b['resident'])))]
   summary={}
   for name,m in [('joint',ctl),('fit_post',fit)]:
    varied=[(r,raw) for r,raw in zip(m.rows,original) if raw['operation']['group'] in ('history_no_haves','history_with_haves')]
    fetch=[r for r,raw in zip(m.rows,original) if raw['operation']['kind']=='fetch']
    summary[name]=dict(stats=dict(m.stats),fetch_gets=sum(r['gets'] for r in fetch),fetch_bytes=sum(r['read_bytes'] for r in fetch),varied_fetch_bytes=sum(r['read_bytes'] for r in fetch),varied_fetch_http_response_bytes=sum(whole(raw['http_response_body_bytes']) for raw in original if raw['operation']['kind']=='fetch'),historical_only_bytes=sum(r['read_bytes'] for r,_ in varied),rows_sha256=sha(json.dumps(m.rows,sort_keys=True).encode()))
   entry=dict(profile=profile,sample=sample,stress_bytes=stress,policies=summary,avoided_gets=ctl.stats['gets']-fit.stats['gets'],avoided_bytes=ctl.stats['bytes']-fit.stats['bytes'],skips=fit.skips,regression_rows=regressions,joint_residents_missing_from_fit_rows=no_superset,lost_response_hits=ctl.stats['response_hits']-fit.stats['response_hits'])
   conditional.setdefault(str(stress),[]).append(entry)
   if sample==0 and regressions:negative.append(entry)
   if stress==MiB:traces[f'{profile}/{sample}']['conditional_1MiB']=dict(joint=ctl.rows,fit_post=fit.rows)

for path in ('examples/repository-host/src/lib.rs','examples/repository-host/src/first_read.rs'):
 b=subprocess.check_output(['git','show',COMMIT+':'+path],cwd=ROOT)
 sources.append(dict(path=path,commit=COMMIT,bytes=len(b),sha256=sha(b)))

# Guard names retain legacy "varied" spelling, but interleaved-v1 includes
# ALL 56 timed fetches: latest, history with/without haves and rechecks.
r19score=json.load(open(audit['inputs'][0]['path']));r18score=json.load(open(audit['inputs'][1]['path']))
guardproof=[];alternative=[];cross=[];costproof=[]
for p in audit['inputs'][-4:]:
 report=json.load(open(p['path']));profile=pathlib.Path(p['path']).stem
 guard_trials=[];cost_trials=[]
 for sample in range(2):
  rows=original_rows(report,sample);fetch=[r for r in rows if r['operation']['kind']=='fetch'];assert len(fetch)==56
  final=next(x for x in report['cloudflare_runs'] if x['sample']==sample)['final_object_stats'];cf=final['cloudflare_services']
  gb=dict(do_sqlite_bytes=cf['do_sqlite_bytes'],r2_live_bytes=cf['r2_live_bytes'],r2_peak_bytes=cf['r2_peak_bytes'],fixed_r2_read_bytes=sum(r['cloudflare_workload_delta']['r2_read_bytes'] for r in rows),fixed_r2_written_bytes=sum(r['cloudflare_workload_delta']['r2_written_bytes'] for r in rows),varied_fetch_r2_read_bytes=sum(r['cloudflare_workload_delta']['r2_read_bytes'] for r in fetch),varied_fetch_http_response_bytes=sum(r['http_response_body_bytes'] for r in fetch),varied_fetch_client_pack_bytes=sum(r['client_received_pack_bytes'] for r in fetch))
  gb['varied_fetch_read_amplification']=gb['varied_fetch_r2_read_bytes']/gb['varied_fetch_http_response_bytes'];guard_trials.append(gb)
  counts={k:sum(r['cloudflare_workload_delta'][k] for r in rows) for k in ('r2_class_a','r2_class_b','worker_requests','do_requests','do_rows_read','do_rows_written')}
  cost=(counts['r2_class_a']*.0045+counts['r2_class_b']*.00036+counts['worker_requests']*.00030+counts['do_requests']*.00015+counts['do_rows_read']*.000001+counts['do_rows_written']*.001+gb['r2_peak_bytes']*.015/1_000_000_000)
  expected=next(t['known_monthly_fixed_workload_usd'] for t in r19score['profiles'][profile]['trials'] if t['sample']==sample);assert abs(cost-expected)<1e-12;cost_trials.append(cost)
  seen=set()
  for r in fetch:
   op=r['operation'];ident=(op['want'],op['have'])
   if ident in seen and op['group']!='latest_recheck':
    body=whole(r['http_response_body_bytes'])-advert_body(op['generation'],report);eligible=body<=ENTRY
    cross.append(dict(profile=profile,sample=sample,operation=op['key'],body_bytes=body,size_eligible=eligible,gets=r['cloudflare_workload_delta']['r2_class_b'] if eligible else 0,extra_DO_above_two_gateway_authorities=max(0,r['cloudflare_workload_delta']['do_requests']-2) if eligible else 0,all_SQL_reads=r['cloudflare_workload_delta']['do_rows_read'] if eligible else 0))
   seen.add(ident)
 for guard in guard_trials[0]:
  actual=sum(t[guard] for t in guard_trials)/2;score=r19score['profiles'][profile]['guards'][guard]['mean'];assert abs(actual-score)<1e-9
  old=r18score['profiles'][profile]['guards'][guard]['mean']
  guardproof.append(dict(profile=profile,guard=guard,independently_rebuilt_R19_mean=actual,R18_mean=old,pass_1_percent=actual<=old*1.01+1e-12))
 cost_mean=sum(cost_trials)/2;old=r18score['profiles'][profile]['known_monthly_fixed_workload_usd']['mean']
 costproof.append(dict(profile=profile,R19_mean=cost_mean,R18_mean=old,pass_1_percent=cost_mean<=old*1.01))
 if profile.endswith('cold'):
  for policy in ('oldest_secondary','oldest_all','largest'):
   m=run(report,0,policy,MiB);joint=run(report,0,'joint',MiB)
   fetch_indices=[i for i,r in enumerate(original_rows(report,0)) if r['operation']['kind']=='fetch'];fetch_bytes=sum(m.rows[i]['read_bytes'] for i in fetch_indices);joint_fetch_bytes=sum(joint.rows[i]['read_bytes'] for i in fetch_indices)
   alternative.append(dict(profile=profile,policy=policy,avoided_gets=joint.stats['gets']-m.stats['gets'],GETs=m.stats['gets'],read_bytes=m.stats['bytes'],joint_read_bytes=joint.stats['bytes'],read_regression_fraction=m.stats['bytes']/joint.stats['bytes']-1,read_guard_pass=m.stats['bytes']<=joint.stats['bytes']*1.01,all_56_fetch_read_bytes=fetch_bytes,joint_all_56_fetch_read_bytes=joint_fetch_bytes,fetch_read_and_amplification_guard_pass=fetch_bytes<=joint_fetch_bytes*1.01))
crosssum={}
for sample in range(2):
 rows=[r for r in cross if r['sample']==sample];eligible=[r for r in rows if r['size_eligible']]
 gets=sum(r['gets'] for r in eligible);do=sum(r['extra_DO_above_two_gateway_authorities'] for r in eligible);sql=sum(r['all_SQL_reads'] for r in eligible)
 crosssum[sample]=dict(logical_repeat_rows=len(rows),size_eligible_rows=len(eligible),actual_eligible_GETs=gets,actual_extra_DO_requests=do,all_SQL_reads=sql,direct_ceiling_usd=gets*.00036+do*.00015+sql*.000001,exact_request_identity_proved=False,feedback_bound_proved=False)
out=dict(kind='independent fit POST design review offline calculations',utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),source_commit=COMMIT,inputs=inputs,source_blobs=sources,exact_advertisement_body_first_generation=438,exact_cloned_response_overhead=dict(discovery=DISC_O,upload_POST=POST_O),controls=results,all_704_controls_match=all(r['all_counter_owning_checks_match'] for r in results),validated_rows=sum(r['rows'] for r in results),validated_push_inventories=sum(r['push_inventories'] for r in results),conditional=conditional,traces=traces,negative_stress=negative,rejected_pressure_alternatives=alternative,independent_actual_36_guard_checks=guardproof,independent_actual_profile_costs=costproof,cross_generation_rows=cross,cross_generation_summary=crosssum,assumptions=['Offline inferred order from immutable append/compaction/lineage semantics; row boundaries are not a complete ordered RPC/control trace.','Conditional stress replaces unobserved metadata/transient charges; it is not allocated reservation.','Exact response upper clone capacity and old/new ring path, socket rotation, mandatory pending publication/metadata pressure remain real source/proof gates.','Source and protocol gates precede any real production benchmark; no implementation GO here.'])
BASE.with_suffix('.calculation.json').write_text(json.dumps(out,sort_keys=True,indent=2)+'\n')
print(json.dumps(dict(all_controls_match=out['all_704_controls_match'],controls=[dict(profile=r['profile'],sample=r['sample'],errors=len(r['mismatches']),first_errors=r['mismatches'][:2]) for r in results]),indent=2))
for stress,entries in conditional.items():
 entries=[e for e in entries if e['sample']==0]
 print(stress,'avoided_GET',sum(e['avoided_gets'] for e in entries),'avoided_bytes',sum(e['avoided_bytes'] for e in entries),'skips',sum(len(e['skips']) for e in entries),'regressions',sum(len(e['regression_rows']) for e in entries))
