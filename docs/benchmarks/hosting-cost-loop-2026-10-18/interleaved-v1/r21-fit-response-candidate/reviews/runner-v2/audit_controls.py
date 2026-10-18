import pathlib,json,collections,datetime
out=pathlib.Path('/tmp/git-zero-fit-response-independent-review-supplement-01')
raw=pathlib.Path('/dev/shm/git-zero-fit-response-correctness-20261018')
load=lambda p:json.load(open(p))
a=load(raw/'allocation-fit-v1-02/allocation-contract.json');f=load(raw/'faults-fit-v1/fault-contract.json');errors=[]
admitted=a['admitted'];pressure=a['pressure']
print('admitted keys',list(admitted))
print('pressure row keys',list(pressure[0]))
print('admitted scalar fields',{k:v for k,v in admitted.items()if not isinstance(v,(dict,list))})
print('admitted metrics charge',admitted.get('metrics',{}).get('cache_retained_bytes'))
print('pressure summary',{'rows':len(pressure),'response_bytes':sorted({r['response_bytes']for r in pressure}),'max_peak':max(r['request_peak_live']for r in pressure),'protected_bytes':sorted({r['after']['protected_chunk_bytes']for r in pressure}),'max_response_entries':max(r['after']['cache_entries']for r in pressure),'total_skip_delta':sum(r['after']['upload_response_retention_skips']-r['before']['upload_response_retention_skips']for r in pressure),'total_pressure_eviction_delta':sum(r['after']['protected_chunk_pressure_evictions']-r['before']['protected_chunk_pressure_evictions']for r in pressure)})
first=load(raw/'allocation-fit-v1-02/first-allocation.json')
allocation={'first':{k:v for k,v in first.items()if not isinstance(v,(dict,list))},'conservative_cache_charge':first['metrics']['cache_retained_bytes'],'retained_within_charge':first['delta']<=first['metrics']['cache_retained_bytes'],'pressure_rows':len(pressure),'response_bytes':sorted({r['response_bytes']for r in pressure}),'max_pressure_request_peak_live':max(r['request_peak_live']for r in pressure),'protected_bytes':sorted({r['after']['protected_chunk_bytes']for r in pressure}),'response_entries':sorted({r['after']['cache_entries']for r in pressure}),'skips_each':[r['after']['upload_response_retention_skips']-r['before']['upload_response_retention_skips']for r in pressure],'pressure_eviction_deltas':[r['after']['protected_chunk_pressure_evictions']-r['before']['protected_chunk_pressure_evictions']for r in pressure]}
for stage in ['pre','post']:
 r=load(raw/f'allocation-fit-v1-02.resource-{stage}.json');allocation['resource_'+stage]={'actual':r,'passes_floor_and_forecast':r['MemAvailable']>=42949672960 and r['tmp_available']>=2147483648 and max(r['own_prefix_allocated_bytes'],r['own_prefix_logical_bytes'])+r['scope_forecast']<=8589934592}
followups=[];attempts=[]
for r in f['rows']:
 if r['kind']=='same Native distinct next push':
  ops=r['trace']['trace'];put=next(i for i,op in enumerate(ops)if op['name']=='r2_put');gc=any(op['name']=='r2_list'and op['route']=='/_services/gc'for op in ops[:put]);excluded=r['mode']in ['unknown-sql','gateway-lost-reply'];followups.append({'mode':r['mode'],'gc_list_before_first_put':gc,'assertion_excluded':excluded,'retained_bytes':r['follow']['cache_retained_bytes'],'peak_bytes':r['follow']['joint_cache_retained_peak_bytes'],'entries':r['follow']['joint_cache_entries']})
  if not gc and not excluded:errors.append({'mode':r['mode'],'error':'missing GC before PUT'})
 if r['kind']=='publication attempt':
  ops=r['trace']['trace'];cas=sum(op['name']=='manifest_cas'for op in ops);attempts.append({'mode':r['mode'],'attempt':r['attempt'],'fired':r['fired'],'status':r['status'],'manifest_cas_attempts':cas})
  if r['fired']and cas>1:errors.append({'mode':r['mode'],'error':'CAS replay'})
result={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'errors':errors,'allocation':allocation,'faults':{'rows':len(f['rows']),'kinds':dict(collections.Counter(r['kind']for r in f['rows'])),'publication_attempts':attempts,'fired_publication_modes':sorted({r['mode']for r in f['rows']if r['kind']=='publication attempt'and r['fired']}),'same_Native_followups':followups,'original_overall_status':'FAIL, not changed by completed rows'}}
(out/'controls-semantic-audit.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'errors':errors,'allocation':allocation,'fault_followups':followups},indent=2))
