import json,pathlib,math,statistics,hashlib,functools
root=pathlib.Path('/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/chain-screen/run-39084173778')
score=json.loads((root/'scores/r5-score.json').read_text());scopes=json.loads((root/'counter-scopes.json').read_text());cap=16*1024*1024
rates={'worker_requests':.30/1e6,'do_requests':.15/1e6,'r2_class_a':4.5/1e6,'r2_class_b':.36/1e6,'do_rows_read':.001/1e6,'do_rows_written':1/1e6}
def price(x,peak=0):return sum(x[k]*v*1000 for k,v in rates.items())+peak/1e9*.015
def chunks(packs):return [min(cap,p['bytes']-i)for p in packs for i in range(0,p['bytes'],cap)]
def greedy(xs):
 n=used=keys=0
 for x in xs:
  if not keys or used+x>cap or keys==256:n+=1;used=keys=0
  used+=x;keys+=1
 return n
@functools.cache
def optimal(original):
 xs=tuple(sorted(original,reverse=True))
 if not xs:return 0
 @functools.cache
 def fits(i,remaining):
  if i==len(xs):return True
  seen=set()
  for j,r in enumerate(remaining):
   if r<xs[i]or r in seen:continue
   seen.add(r);a=list(remaining);a[j]-=xs[i]
   if fits(i+1,tuple(sorted(a,reverse=True))):return True
  return False
 for n in range(max(math.ceil(sum(xs)/cap),sum(x>cap/2 for x in xs)),greedy(xs)+1):
  if fits(0,(cap,)*n):return n
 raise ValueError(xs)
def sub(a,b):return {k:None if v is None else v-b.get(k,0)for k,v in a.items()}
def sum_counters(rows):
 return {k:None if rows[0][k] is None else sum(r[k]for r in rows)for k in rows[0]}
result={'kind':'offline_counter_projection_not_measurement','source_run':39084173778,'source_anchor':'R5','read_source_head':'b2df9560c7461fee85dc6433c2fdf97437b196fb','physical_chunk_bytes':cap,'bulk_payload_cap':cap,'no_network_build_benchmark_or_service_probe':True,'candidate':'ephemeral conservative historical prefix lineage','rates_snapshot':'existing cost.rs 2026-10-17; not refreshed pricing','rates_usd_per_one':rates,'workload_repetitions':1000,'storage_days':30,'scope_contract':{'fixed':'exact measured 32 pushes + 56 fetches; observer excluded','legacy_whole_nonstats':'startup and all observed non-stats traffic; existing score whole_trace_observer_excluded','all_observed_physical':'final_object_stats.cloudflare_services; includes observer calls and R2 lists','required_restore':'additional final stats full restore explicitly charged in complete physical scope and separately reported, never hidden by stats exclusion'},'projection_conditions':['Every historical wire request passes conservative full v0 pkt-line classification; original full bodies are not archived.','Same exact HTTP operation counts and response-cache hit pattern; projected content/transfer/eviction guards require measurement.','Lineage, advertisement and response allocations jointly fit existing 32 MiB budget; lineage never persisted.','Strict object-preserving publication and serial request model continue.','Bulk-read DO constructor/eviction row effects remain actual measured observations, not assumed billed CPU.'],'profiles':{}}
for name in ['16mib-cold','16mib-warm','64mib-cold','64mib-warm']:
 path=root/'profiles'/name/'r5'/f'{name}.json';doc=json.loads(path.read_text());cold=name.endswith('cold');out=[]
 for sample in [0,1]:
  rows=[r for r in doc['raw']if r['backend']=='cloudflare-local'and not r['warmup']and r['sample']==sample];assert len(rows)==88
  current=[];snapshots={0:[]};ops=[];cachebytes=peakcache=best_delta=best_r8_delta=0
  zero={k:0 for k,v in rows[0]['cloudflare_workload_delta'].items()if v is not None};fixed_saved=zero.copy()
  gens={e['tip']:e['generation']for e in doc['fixture']['generations']}
  for row in rows:
   op=row['operation'];g=op['generation'];actual=row['cloudflare_workload_delta'];ms=row['service_metrics_delta'];cachebytes+=int(ms.get('cache_bytes',0));peakcache=max(peakcache,cachebytes)
   if op['kind']=='push':
    rehydrates=int(cold);count=int(row['compaction_delta']['count']);written=actual['r2_written_bytes']
    if count:
     chosen=[p for p in current if p['level']==0 and p['created']!=1];assert len(chosen)==3 and g in [5,9,13,17,21,25,29];assert actual['r2_deletes']==len(chosen)
     floor=min([p['floor']for p in chosen]+[g]);nextpacks=[p for p in current if p not in chosen]
    else:floor=g;nextpacks=list(current)
    nextpacks.append({'created':g,'floor':floor,'level':1 if count else 0,'bytes':written})
   else:rehydrates=int(ms.get('cloud_view_rehydrates',0))if cold else 0
   xs=chunks(current);gs=greedy(xs);opt=optimal(tuple(xs))
   assert actual['r2_class_b']==rehydrates*len(xs),(name,sample,row['workload'],'GET count')
   assert actual['r2_read_bytes']==rehydrates*sum(xs),(name,sample,row['workload'],'read bytes')
   best_delta+=rehydrates*(gs-opt);r8rh=rehydrates-int(cold and op['group']=='latest_fetch'and g>1);best_r8_delta+=r8rh*(gs-opt)
   selected=current;cutoff=None
   if cold and op['group']in ['history_no_haves','history_with_haves']:
    cutoff=max(gens[x]for x in [op['want'],op['have']]if x is not None);selected=[p for p in current if p['floor']<=cutoff]
   ys=chunks(selected);saved=zero.copy();saved['r2_class_b']=rehydrates*(len(xs)-len(ys));saved['r2_read_bytes']=rehydrates*(sum(xs)-sum(ys));cb=rehydrates*(gs-greedy(ys))
   for k in ['worker_requests','worker_service_requests','do_requests']:saved[k]=cb
   fixed_saved={k:fixed_saved[k]+saved[k]for k in zero}
   ops.append({'sample':sample,'operation':row['workload'],'group':op['group'],'generation':g,'cutoff':cutoff,'whole_snapshot_bytes':sum(xs),'full_chunk_gets_per_rehydrate':len(xs),'full_bridge_reads_per_rehydrate':gs,'optimal_unordered_bridge_reads_per_rehydrate':opt,'selected_snapshot_bytes':sum(ys),'selected_chunk_gets_per_rehydrate':len(ys),'selected_bridge_reads_per_rehydrate':greedy(ys),'actual_rehydrates':rehydrates,'actual_workload_counters':actual,'projected_workload_counters':sub(actual,saved),'known_priced_counter_savings':saved,'actual_http_output_bytes':ms.get('output_bytes'),'projected_http_output_bytes':None,'actual_response_cache_hits':ms.get('cache_hits'),'actual_discovery_cache_hits':ms.get('discovery_cache_hits')})
   if op['kind']=='push':current=nextpacks;snapshots[g]=[dict(p)for p in current]
  run=next(r for r in doc['cloudflare_runs']if r['sample']==sample and not r['warmup']);final=run['final_object_stats'];mf=final['manifest'];fchunks=chunks(current)
  assert [p['bytes']for p in current]==[p['pack_bytes']+p['index_bytes']for p in mf['packs']]
  actualfixed=sum_counters([r['cloudflare_workload_delta']for r in rows]);assert actualfixed==next(t for t in score['profiles'][name]['trials']if t['sample']==sample)['fixed_workload']
  whole=next(s for s in scopes if s['profile']==name and s['provider']=='r5'and s['sample']==sample)['whole_trace_observer_excluded'];physical={k:final['cloudflare_services'][k]for k in actualfixed};restore=zero.copy()
  if cold:
   restore['r2_class_b']=len(fchunks);restore['r2_read_bytes']=sum(fchunks)
   for k in ['worker_requests','worker_service_requests','do_requests']:restore[k]=greedy(fchunks)
   last=rows[-1];assert last['workload']=='latest_recheck_32'and last['service_metrics_delta']['cache_hits']==1 and last['cloudflare_workload_delta']['r2_read_bytes']==0
  net={k:fixed_saved[k]-restore[k]for k in zero};peak=final['cloudflare_services']['r2_peak_bytes'];fixedcandidate=sub(actualfixed,fixed_saved);physicalcandidate=sub(physical,net);nonstatscandidate=sub(whole,net)
  manifestbytes=len(json.dumps(mf,separators=(',',':')).encode());meta={'history':{'v':1,'p':[p['floor']for p in current],'r':[int(k.rsplit('-',1)[1])if k!='refs/heads/main' else 32 for k in sorted(mf['refs'])]}};extra=len(json.dumps(meta,separators=(',',':')).encode())-1
  prices={'fixed_actual':price(actualfixed,peak),'fixed_projected':price(fixedcandidate,peak),'fixed_saved':price(fixed_saved),'all_observed_physical_actual':price(physical,peak),'all_observed_physical_projected':price(physicalcandidate,peak),'complete_physical_net_saved':price(net),'legacy_whole_nonstats_actual':price(whole,peak),'legacy_whole_nonstats_plus_required_restore_projected':price(nonstatscandidate,peak),'required_restore':price(restore)}
  out.append({'sample':sample,'reproduced_raw_rows':len(rows),'fixed_actual':actualfixed,'fixed_projected':fixedcandidate,'fixed_savings':fixed_saved,'legacy_whole_nonstats_actual':whole,'all_observed_physical_actual':physical,'all_observed_physical_projected':physicalcandidate,'additional_final_global_scan_restore':restore,'complete_physical_net_savings':net,'prices_usd_per_1000_workloads_peak30d':prices,'unchanged_r2_peak_bytes':peak,'unchanged_do_sqlite_bytes':final['cloudflare_services']['do_sqlite_bytes'],'additional_durable_manifest_bytes':0,'rejected_durable_sidecar_comparison_bytes':extra,'actual_compact_final_manifest_bytes':manifestbytes,'lineage_array_elements_at_generation32':len(current)+len(mf['refs']),'lineage_array_payload_bytes_if_u64':8*(len(current)+len(mf['refs'])),'baseline_response_cache_peak_observed_at_raw_boundaries':peakcache,'bulk_reordering_all_reads_callbacks_avoidable':best_delta,'bulk_reordering_after_r8_callbacks_avoidable':best_r8_delta,'generation_snapshots':snapshots,'operations':ops})
 result['profiles'][name]={'source_report':str(path),'source_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'samples':out}
keys=list(prices);totals={k:sum(statistics.mean(s['prices_usd_per_1000_workloads_peak30d'][k]for s in p['samples'])for p in result['profiles'].values())for k in keys};assert abs(totals['fixed_actual']-score['known_monthly_fixed_workload_usd'])<1e-12
totals['fixed_reduction_fraction']=totals['fixed_saved']/totals['fixed_actual'];totals['complete_physical_reduction_fraction']=totals['complete_physical_net_saved']/totals['all_observed_physical_actual'];totals['legacy_nonstats_with_explicit_restore_reduction_fraction']=(totals['legacy_whole_nonstats_actual']-totals['legacy_whole_nonstats_plus_required_restore_projected'])/totals['legacy_whole_nonstats_actual'];result['aggregate']=totals
path=pathlib.Path('/tmp/git-zero-next-cost-projection.json');path.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(totals,indent=2))
for name,p in result['profiles'].items():
 for s in p['samples']:print(name,s['sample'],'fixedSaved',s['fixed_savings']['r2_class_b'],s['fixed_savings']['r2_read_bytes'],s['fixed_savings']['worker_service_requests'],'restore',s['additional_final_global_scan_restore']['r2_class_b'],s['additional_final_global_scan_restore']['r2_read_bytes'],s['additional_final_global_scan_restore']['worker_service_requests'],'binpackSaved',s['bulk_reordering_all_reads_callbacks_avoidable'],'peakcache',s['baseline_response_cache_peak_observed_at_raw_boundaries'],'meta',s['lineage_array_payload_bytes_if_u64'])
print('WROTE',path)
