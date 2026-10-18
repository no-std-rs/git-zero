"""Offline JSON counter arithmetic only: no source edit, services, tests, or network."""
import hashlib, json
from pathlib import Path
base=Path('/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/chain-screen/run-39084173778')
prior=json.loads(Path('/tmp/git-zero-next-cost-projection.json').read_text())
profiles={}
for profile in ('16mib-cold','16mib-warm','64mib-cold','64mib-warm'):
    path=base/'profiles'/profile/'r5'/f'{profile}.json'
    report=json.loads(path.read_text())
    samples=[]
    for sample in (0,1):
        rows=[r for r in report['raw'] if r['backend']=='cloudflare-local' and not r['warmup'] and r['sample']==sample]
        pushes=[r for r in rows if r['operation']['kind']=='push']
        out=[]
        for row in pushes:
            delta=row['cloudflare_workload_delta']
            operations=row['cloudflare_services_delta']['operations']
            puts=int(operations.get('r2_put',{}).get('attempts',0))
            deletes=int(delta['r2_deletes'])
            cas=int(operations.get('manifest_cas',{}).get('successes',0))
            peak_writes=int(delta['do_rows_written'])-puts-deletes-cas
            assert cas==1 and peak_writes>=0
            out.append({'operation':row['operation']['key'],'actual_put_catalog_writes':puts,'actual_delete_catalog_writes':deletes,'actual_manifest_writes':cas,'inferred_actual_high_water_writes':peak_writes,'observed_peak_before':row['cloudflare_services_before']['r2_peak_bytes'],'observed_peak_after':row['cloudflare_services_after']['r2_peak_bytes'],'observed_live_after':row['cloudflare_services_after']['r2_live_bytes']})
        total={k:sum(row[k] for row in out) for k in ('actual_put_catalog_writes','actual_delete_catalog_writes','actual_manifest_writes','inferred_actual_high_water_writes')}
        fixed_writes=sum(int(r['cloudflare_workload_delta']['do_rows_written']) for r in rows)
        assert fixed_writes==sum(total.values())
        assert len(rows)==88 and len(pushes)==32
        costs=prior['profiles'][profile]['samples'][sample]['prices_usd_per_1000_workloads_peak30d']
        gross=total['inferred_actual_high_water_writes']*.001
        samples.append({'sample':sample,'actual_fixed_sql_writes':fixed_writes,'conditional_fixed_sql_writes_without_fallbacks':fixed_writes-total['inferred_actual_high_water_writes'],'attribution':total,'observed_distinct_do_epochs_at_row_boundaries':len({r['cloudflare_services_after']['do_instance_epoch'] for r in rows}),'fixed_known_usd_actual':costs['fixed_actual'],'fixed_known_usd_conditional_before_added_read_rows_or_migration':costs['fixed_actual']-gross,'whole_nonstats_known_usd_actual':costs['legacy_whole_nonstats_actual'],'all_physical_known_usd_actual':costs['all_observed_physical_actual'],'gross_known_usd_saving_per_1000_replays':gross,'actual_final_peak_bytes':rows[-1]['cloudflare_services_after']['r2_peak_bytes'],'actual_final_sqlite_bytes':rows[-1]['cloudflare_services_after']['do_sqlite_bytes'],'operations':out})
    profiles[profile]={'source_report':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'samples':samples}
# Samples are repetitions of the same four-profile objective, not eight profiles.
agg={k:sum(sum(s[k] for s in p['samples'])/2 for p in profiles.values()) for k in ('actual_fixed_sql_writes','conditional_fixed_sql_writes_without_fallbacks','gross_known_usd_saving_per_1000_replays','fixed_known_usd_actual','whole_nonstats_known_usd_actual','all_physical_known_usd_actual')}
gross=agg['gross_known_usd_saving_per_1000_replays']
agg['gross_fixed_reduction_fraction_before_added_cost']=gross/agg['fixed_known_usd_actual']
agg['gross_whole_nonstats_reduction_fraction_before_added_cost']=gross/agg['whole_nonstats_known_usd_actual']
agg['gross_all_physical_reduction_fraction_before_added_cost']=gross/agg['all_physical_known_usd_actual']
agg['extra_sql_read_rows_per_four_profile_replay_break_even']=gross/.000001
agg['extra_sql_read_rows_per_four_profile_replay_max_for_one_percent_fixed']=(gross-.01*agg['fixed_known_usd_actual'])/.000001
agg['cost_formula_per_1000_replays']='saving_usd = 0.060 - 0.001 * added_rows_written_per_four_profile_replay - 0.000001 * added_rows_read_per_four_profile_replay; operation/storage deltas must separately be measured'
result={'kind':'conditional_offline_counter_projection_not_measurement','candidate':'durable peak watermark carried by already-written PUT catalog row','source_run':39084173778,'source_anchor':'R5','production_source_anchor_reported_by_parent':'0cd459ccedaa52608bd0d81a8a6b4c4683e27712','profiles':profiles,'aggregate':agg,'conditional_added_normal_workload':{'r2_get':0,'r2_head':0,'r2_put':0,'r2_list':0,'r2_delete':0,'r2_read_bytes':0,'r2_written_bytes':0,'worker_requests':0,'do_requests':0,'manifest_bytes':0,'high_water_writes':0,'catalog_rows_written':0,'sql_read_rows':None,'schema_migration_row_writes':None,'sqlite_bytes':None},'conditions':['No owner witness is deleted by normal trace retirement because a successful new output PUT remains live.','Fresh namespace starts v1 under common Worker; candidate first eligible mutation ALTER cost must be measured and charged in its actual phase, including fixed push when applicable.','Constructor/schema discovery, owner recovery scans, and actual cursor rowsRead/rowsWritten must be charged.','No new read-triggered SQL writes or R2 Class A calls; existing metrics inventory LIST remains observer overhead.','Actual SQLite pages pass the unchanged <=1% storage guard.','No inferred saving is substituted for actual receipts.'],'rates_snapshot':'existing repository 2026-10-17 cost.rs, not refreshed provider prices','rates_usd_per_row':{'write':.000001,'read':.000000001},'monthly_replays_of_each_profile':1000,'unmeasured':{'worker_cpu_ms':None,'do_gb_seconds':None,'monthly_sqlite_retention':None,'native_git_compute':None,'full_cloud_bill_usd':None},'combined_r9_or_prefix_score':None,'implementation':False,'benchmarks_or_tests_run':False,'network_or_services_probed':False,'global_optimum_or_plateau_claim':False}
result['activation']={'native_registration_body':{'catalog_peak':1},'common_worker':True,'fresh_legacy_namespaces_stay_v1':True,'migration':'only validated trusted-capability catalog mutation','first_mutation_migration_cost_may_be_fixed':True,'existing_v2_always_preserves_peak_without_registration':True,'extra_registration_do_calls':0,'trusted_header':'x-git-zero-internal-catalog-peak','old_worker_rollback':'requires explicit durable high_water checkpoint; not automatically supported'}
Path('/tmp/git-zero-peak-catalog-projection.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(agg,indent=2))
