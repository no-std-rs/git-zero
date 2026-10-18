import collections
import json
from pathlib import Path

ROOT = Path('/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r20-r21-screen/run-39145424189')
COUNTERS = ['worker_requests', 'worker_gateway_requests', 'worker_service_requests',
            'worker_observer_requests', 'do_requests', 'do_rows_read', 'do_rows_written',
            'r2_class_a', 'r2_class_b', 'r2_deletes', 'r2_read_bytes', 'r2_written_bytes']
RATES = dict(r2_class_a=4.5, r2_class_b=.36, worker_requests=.3,
             do_requests=.15, do_rows_read=.001, do_rows_written=1)
checks = collections.Counter()
trials = []

def check(ok, name, where):
    assert ok, (name, where)
    checks[name] += 1

def snapshot(s, where):
    check(s['accounting_complete'] and not s['accounting_errors'], 'complete', where)
    check(s['worker_requests'] == sum(s[k] for k in
          ['worker_gateway_requests', 'worker_service_requests', 'worker_observer_requests']),
          'worker_categories', where)
    check(s['worker_cpu_ms'] is None and s['do_gb_seconds'] is None, 'compute_unknown', where)
    phases = s['by_phase']
    for k in COUNTERS:
        check(s[k] == sum(p[k] for p in phases.values()), 'phase_' + k, where)
    ops = s['operations']
    for name, r in ops.items():
        check(r['attempts'] == r['successes'] + r['failures'], 'api_attempt_closure', (where, name))
    check(s['r2_class_a'] == sum(ops.get(n, {}).get('attempts', 0) for n in ['r2_put', 'r2_list']), 'actual_class_a', where)
    check(s['r2_class_b'] == sum(ops.get(n, {}).get('attempts', 0) for n in ['r2_get', 'r2_head']), 'actual_class_b', where)
    check(s['r2_deletes'] == ops.get('r2_delete', {}).get('attempts', 0), 'actual_deletes', where)
    check(s['r2_read_bytes'] == sum(ops.get(n, {}).get('read_bytes', 0) for n in ['r2_get', 'r2_head']), 'actual_read_bytes', where)
    check(s['r2_written_bytes'] == ops.get('r2_put', {}).get('written_bytes', 0), 'actual_written_bytes', where)

for profile in ['16mib-cold', '16mib-warm', '64mib-cold', '64mib-warm']:
    for provider in ['r20', 'r21']:
        report = json.loads((ROOT / 'profiles' / profile / provider / (profile + '.json')).read_text())
        for row in report['raw']:
            if row['backend'] != 'cloudflare-local':
                continue
            where = profile, provider, row['sample'], row['workload']
            before, after = row['cloudflare_services_before'], row['cloudflare_services_after']
            snapshot(before, (where, 'before'))
            snapshot(after, (where, 'after'))
            check((before['counter_scope'], before['counter_epoch']) ==
                  (after['counter_scope'], after['counter_epoch']), 'stable_counter_scope', where)
            delta, fixed = row['cloudflare_services_delta'], row['cloudflare_workload_delta']
            for k in COUNTERS:
                check(delta[k] == after[k] - before[k], 'actual_row_delta_' + k, where)
                observer_delta = after['by_phase'].get('stats', {}).get(k, 0) - before['by_phase'].get('stats', {}).get(k, 0)
                check(fixed[k] == delta[k] - observer_delta, 'actual_fixed_excludes_stats_' + k, where)
            check(fixed['worker_cpu_ms'] is None and fixed['do_gb_seconds'] is None, 'fixed_compute_unknown', where)
        for run in report['cloudflare_runs']:
            where = profile, provider, run['sample'], run['warmup']
            services = run['final_object_stats']['cloudflare_services']
            snapshot(services, where)
            rows = [r for r in report['raw'] if r['backend'] == 'cloudflare-local' and r['sample'] == run['sample'] and r['warmup'] == run['warmup']]
            fixed = {k: sum(r['cloudflare_workload_delta'][k] for r in rows) for k in COUNTERS}
            startup = services['by_phase']['startup']
            stats = services['by_phase']['stats']
            observed = run['estimated_service_costs']['observed_workload']
            for k in COUNTERS:
                check(services[k] - stats[k] == observed[k] == fixed[k] + startup[k], 'whole_fixed_startup_' + k, where)
            trials.append(dict(profile=profile, provider=provider, sample=run['sample'], warmup=run['warmup'],
                               measured_operations=len(rows), actual_fixed=fixed, actual_startup=startup,
                               actual_excluded_stats=stats, actual_whole_nonstats=observed,
                               startup_known_request_usd_for_1000_repetitions=sum(startup[k]*v*.001 for k,v in RATES.items()),
                               CPU_DO_duration_full_bill=None))

assert len(trials) == 24
result = dict(run=39145424189, attempt=1, checks=dict(checks), violations=[], trials=trials,
              scope='Root independently recomputed actual API/category/phase/row/fixed/startup/observer closure from original reports including all warmups; no invented Worker to DO invocation ratio.',
              full_bill=None, unknown_dimensions=['Worker CPU', 'DO duration', 'monthly SQLite storage', 'Native compute'])
out = Path('/tmp/git-zero-r20-r21-fresh-root-service-scope-review.json')
out.write_text(json.dumps(result, indent=2) + '\n')
print('ROOT PASS', sum(checks.values()), 'service scope checks across all 24 warmup/measured trials:', out)
