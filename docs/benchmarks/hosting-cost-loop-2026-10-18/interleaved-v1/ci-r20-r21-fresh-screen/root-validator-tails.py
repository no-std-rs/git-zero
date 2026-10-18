import json,math,sys
from pathlib import Path
run_id=int(sys.argv[1]); r=Path(f'/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r20-r21-screen/run-{run_id}');data={};gits={}
for p in (r/'profiles').glob('*/*/*.json'):
 if p.stem!=p.parent.parent.name:continue
 report=json.loads(p.read_text());profile=p.parent.parent.name;provider=p.parent.name
 for row in report['raw']:
  if row['warmup'] or not row.get('operation'):continue
  (data if row['backend']=='cloudflare-local' else gits)[profile,provider,row['sample'],row['workload']]=row
rows=json.loads((r/'retained-tail-summary.json').read_text())['tails'];assert len(rows)==64
for tail in rows:
 key=tail['profile'],tail['provider'],tail['sample'],tail['workload'];raw=data[key];git=gits[key]
 for k in ['operation','wall_ms','ready_wall_ms','client_resources','push_wire_bytes','client_received_pack_bytes','http_response_body_bytes','run_contention_before','run_contention_after']:assert tail[k]==raw.get(k),(key,k)
 # Compact tails retain scalar Native deltas; full nested service/view objects remain in original reports.
 for k,v in tail['service_metrics_delta'].items():assert v==raw['service_metrics_delta'][k]
 assert tail['fixed_workload_receipt']==raw['cloudflare_workload_delta']
 timer='ready_wall_ms' if raw['operation']['kind']=='push' else 'wall_ms';assert tail['paired_git_ready_or_fetch_ms']==git[timer];assert math.isclose(tail['paired_host_to_git_ratio'],raw[timer]/git[timer],rel_tol=1e-12)
for profile,provider in {(k[0],k[1]) for k in data}:
 expected=sorted((row['wall_ms'] for k,row in data.items() if k[:2]==(profile,provider)),reverse=True)[:8];actual=sorted((tail['wall_ms'] for tail in rows if tail['profile']==profile and tail['provider']==provider),reverse=True);assert expected==actual
failed=json.loads((r/'latency-failure-pair-details.json').read_text());n=0
for failure in failed:
 for pair in failure['all_baseline_and_candidate_pairs']:
  key=failure['profile'],pair['provider'],pair['sample'],pair['operation']['key'];host=data[key];git=gits[key];timer='ready_wall_ms' if host['operation']['kind']=='push' else 'wall_ms'
  assert pair['operation']==host['operation'];assert pair['host_ms']==host[timer] and pair['git_ms']==git[timer];assert math.isclose(pair['ratio'],host[timer]/git[timer],rel_tol=1e-12);assert pair['fixed_receipt']==host['cloudflare_workload_delta'];n+=1
out=Path('/tmp/git-zero-r20-r21-fresh-root-guard-review.json');x=json.loads(out.read_text());x['root_independently_rebuilt_from_original_reports'].update(all64_published_tail_rows_selected_metrics_equal_original_and_paired_git=True,all8_profile_top8_tail_selections_correct=True,all_failed_latency_groups_every_individual_pair_verified=n);out.write_text(json.dumps(x,indent=2)+'\n');print('ROOT PASS:64tailrows/scalarNative+fullfixedreceipt/rawpairs/top8selections and',n,'pairs from all failed groups.')
