import json, math, statistics, sys
from pathlib import Path
run_id=int(sys.argv[1]); head=sys.argv[2]
root=Path(f'/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r20-r21-screen/run-{run_id}')

def stats(v):
 v=sorted(v)
 return dict(mean=sum(v)/len(v),min=v[0],max=v[-1],median=statistics.median(v),p95=v[math.ceil(.95*len(v))-1])
def same(a,b):
 if isinstance(a,dict):
  assert a.keys()==b.keys(),(a.keys(),b.keys())
  for k in a: same(a[k],b[k])
 elif isinstance(a,(float,int)) and not isinstance(a,bool): assert math.isclose(a,b,rel_tol=1e-12,abs_tol=1e-8),(a,b)
 else: assert a==b,(a,b)
reports={}; rebuilt={}; allratios=[]; tails=[]
for profile in ['16mib-cold','16mib-warm','64mib-cold','64mib-warm']:
 for provider in ['r20','r21']:
  p=root/'profiles'/profile/provider/(profile+'.json')
  if not p.is_file(): continue
  report=json.loads(p.read_text());assert report['complete'] is True
  rows=[x for x in report['raw'] if not x['warmup']]
  groups={}; resources={};samples={}
  for sample in [0,1]:
   host=[x for x in rows if x['backend']=='cloudflare-local' and x['sample']==sample]
   git={x['workload']:x for x in rows if x['backend']=='git' and x['sample']==sample and x.get('operation')}
   assert len(host)==len(git)==88
   samplevalues={k:0 for k in ['fixed_r2_read_bytes','fixed_r2_written_bytes','varied_fetch_r2_read_bytes','varied_fetch_http_response_bytes','varied_fetch_client_pack_bytes']}
   for h in host:
    g=git[h['workload']];operation=h['operation'];assert operation==g['operation']
    group=operation['group'];t='ready_wall_ms' if operation['kind']=='push' else 'wall_ms'
    record=dict(profile=profile,provider=provider,sample=sample,workload=h['workload'],group=group,host_ms=h[t],git_ms=g[t],ratio=h[t]/g[t])
    allratios.append(record);groups.setdefault(group,[]).append(record)
    for k,deltakey in [('fixed_r2_read_bytes','r2_read_bytes'),('fixed_r2_written_bytes','r2_written_bytes')]: samplevalues[k]+=h['cloudflare_workload_delta'][deltakey]
    if operation['kind']=='fetch':
     samplevalues['varied_fetch_r2_read_bytes']+=h['cloudflare_workload_delta']['r2_read_bytes']
     samplevalues['varied_fetch_http_response_bytes']+=h['http_response_body_bytes']
     samplevalues['varied_fetch_client_pack_bytes']+=h['client_received_pack_bytes']
   last=host[-1]['cloudflare_services_after']
   for k in ['r2_peak_bytes','r2_live_bytes','do_sqlite_bytes']: samplevalues[k]=last[k]
   samplevalues['varied_fetch_read_amplification']=samplevalues['varied_fetch_r2_read_bytes']/samplevalues['varied_fetch_http_response_bytes']
   for k,v in samplevalues.items(): resources.setdefault(k,[]).append(v)
  rebuilt[profile,provider]=dict(resources={k:stats(v) for k,v in resources.items()},latency={k:{'host_ms':stats([x['host_ms'] for x in v]),'git_ms':stats([x['git_ms'] for x in v]),'paired_host_to_git':stats([x['ratio'] for x in v])} for k,v in groups.items()})
  tails.extend(max(v,key=lambda x:x['host_ms']) for v in groups.values())
  reports[profile,provider]=p

scores={p:json.loads((root/'scores'/(p+'-score.json')).read_text()) for p in ['r20','r21']}
known={}
for (profile,provider),values in rebuilt.items():
 score=scores[provider]['profiles'][profile]
 same(values['resources'],score['guards']);same(values['latency'],score['latency'])
 raw=json.loads(reports[profile,provider].read_text())['raw'];costs=[]
 for sample in [0,1]:
  rows=[r for r in raw if not r['warmup'] and r['sample']==sample and r['backend']=='cloudflare-local']
  rates={'r2_class_a':4.5,'r2_class_b':.36,'worker_requests':.3,'do_requests':.15,'do_rows_read':.001,'do_rows_written':1.0}
  totals={k:sum(r['cloudflare_workload_delta'][k] for r in rows) for k in rates}
  claimed=score['trials'][sample];
  for k,v in totals.items():assert v==claimed['fixed_workload'][k]
  price=sum(totals[k]*v*.001 for k,v in rates.items())+rows[-1]['cloudflare_services_after']['r2_peak_bytes']/1e9*.015
  same(price,claimed['known_monthly_fixed_workload_usd']);costs.append(price)
 known.setdefault(provider,{})[profile]=stats(costs)
for provider,profiles in known.items():same(sum(v['mean'] for v in profiles.values()),scores[provider]['known_monthly_fixed_workload_usd'])
resource_n=latency_n=failed_groups=failed_limits=profilecost_n=0;comparisons=[]
for baseline,candidate in [('r20','r21')]:
 name=candidate+'-vs-'+baseline;ci=json.loads((root/'scores'/(name+'.json')).read_text());assert ci['comparison']['decision']=='inconclusive' and not ci['comparison']['eligible_rejected_candidate_for_plateau'];failures=[]
 cost_separated=scores[candidate]['conservative_trial_max_usd']<=scores[baseline]['conservative_trial_min_usd']*.99;resource_failures=[];profile_cost_failures=[]
 for profile in ['16mib-cold','16mib-warm','64mib-cold','64mib-warm']:
  b=rebuilt[profile,baseline];c=rebuilt[profile,candidate]
  profilecost_n+=1
  if known[candidate][profile]['mean']>known[baseline][profile]['mean']*1.01:profile_cost_failures.append(dict(profile=profile,baseline=known[baseline][profile],candidate=known[candidate][profile]))
  for k,v in b['resources'].items():
   resource_n+=1
   if c['resources'][k]['mean']>v['mean']*1.01:resource_failures.append(dict(profile=profile,metric=k,baseline=v,candidate=c['resources'][k]))
  for group,v in b['latency'].items():
   br=v['paired_host_to_git'];cr=c['latency'][group]['paired_host_to_git'];medianpass=cr['median']<=br['median']*1.10;p95pass=cr['p95']<=br['p95']*1.25;latency_n+=1;failed_limits+=not medianpass;failed_limits+=not p95pass
   if not(medianpass and p95pass):
    failed_groups+=1;failures.append(dict(profile=profile,group=group,median_pass=medianpass,p95_pass=p95pass,baseline=br,candidate=cr))
    assert any(reason.startswith(profile+'/'+group+':') for reason in ci['comparison']['reasons'])
  assert scores[candidate]['profiles'][profile]['total_estimate_usd'] is None
 comparisons.append(dict(comparison=name,known_cost_reduction_fraction=ci['comparison']['known_cost_reduction_fraction'],cost_separated_by_at_least_one_percent=cost_separated,failed_resource_guards=resource_failures,failed_profile_cost_guards=profile_cost_failures,failed_latency_groups=failures,formal_decision='inconclusive',plateau_eligible=False))
assert len(reports)==8 and len(allratios)==1408 and resource_n==36 and latency_n==24 and profilecost_n==4
out={'run':run_id,'attempt':1,'head':head,'root_independently_rebuilt_from_original_reports':{'reports':len(reports),'ratios':len(allratios),'resources':resource_n,'profile_cost_guards':profilecost_n,'latency_groups':latency_n,'latency_limits':latency_n*2,'failed_latency_groups':failed_groups,'failed_latency_limits':failed_limits,'every_group_maximum_tail':len(tails),'manual_known_pricing_matches_all16trials_and2scores':True},'known_costs':{p:scores[p]['known_monthly_fixed_workload_usd'] for p in scores},'comparisons':comparisons,'worst_operation':max(allratios,key=lambda x:x['host_ms']),'worst_operation_by_provider':{p:max([a for a in allratios if a['provider']==p],key=lambda x:x['host_ms']) for p in scores},'limits':['Screens are Inconclusive and plateau-ineligible; no speed or formalpromotion.','Only provisional known-cost anchor may be selected afterallsource/guard/tail/scope reviews; fullbillNULL.','MandatoryfiveFRESHoriginalR1/finalpairsallfourprofiles remains undone.']}
p=Path('/tmp/git-zero-r20-r21-fresh-root-guard-review.json');p.write_text(json.dumps(out,indent=2)+'\n');print('ROOT PASS',p,'16manualpricedtrials/2scores/36resource4profilecost/24latency48limits/1408ratios/dynamicfailedgroupslimits/48groupmaxima');print('known',out['known_costs']);print('worst_by_provider',out['worst_operation_by_provider'])
