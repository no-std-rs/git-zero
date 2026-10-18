"""Offline audit of archived JSON only; never starts a service or workload."""
import collections
import functools
import hashlib
import json
import math
import pathlib

ROOT = pathlib.Path('/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r5-r8-screen/run-39087721879')
CAP = 16 * 1024 * 1024

def greedy(xs):
    bins = used = keys = 0
    for x in xs:
        if not keys or used + x > CAP or keys == 256:
            bins += 1
            used = keys = 0
        used += x
        keys += 1
    return bins

@functools.cache
def optimal(original):
    xs = tuple(sorted(original, reverse=True))
    if not xs:
        return 0
    @functools.cache
    def fits(i, remaining):
        if i == len(xs):
            return True
        seen = set()
        for j, room in enumerate(remaining):
            if room < xs[i] or room in seen:
                continue
            seen.add(room)
            next_rooms = list(remaining)
            next_rooms[j] -= xs[i]
            if fits(i + 1, tuple(sorted(next_rooms, reverse=True))):
                return True
        return False
    lower = max(math.ceil(sum(xs) / CAP), sum(x > CAP // 2 for x in xs))
    for bins in range(lower, greedy(xs) + 1):
        if fits(0, (CAP,) * bins):
            return bins
    raise AssertionError(xs)

def chunks(packs):
    return [min(CAP, p['bytes'] - i) for p in packs for i in range(0, p['bytes'], CAP)]

score_path = ROOT / 'scores/r8-score.json'
score = json.loads(score_path.read_text())
result = {
    'kind': 'independent_offline_file_arithmetic_not_measurement',
    'source': str(ROOT),
    'source_score_sha256': hashlib.sha256(score_path.read_bytes()).hexdigest(),
    'source_anchor': 'R8 measured screen; later revisions remain projections',
    'rates_snapshot': 'cost.rs dated 2026-10-17, no fresh pricing lookup',
    'fixed_known_usd': score['known_monthly_fixed_workload_usd'],
    'profiles': {},
}
resources = collections.defaultdict(float)
all_checks = 0
for name, profile in score['profiles'].items():
    report_path = ROOT / 'profiles' / name / 'r8' / f'{name}.json'
    doc = json.loads(report_path.read_text())
    cold = name.endswith('cold')
    generations = {g['tip']: g['generation'] for g in doc['fixture']['generations']}
    samples = []
    for trial in profile['trials']:
        for k, v in trial['resource_monthly_usd'].items():
            resources[k] += v / len(profile['trials'])
        rows = [r for r in doc['raw'] if r['backend'] == 'cloudflare-local' and not r['warmup'] and r['sample'] == trial['sample']]
        assert len(rows) == 88
        current = []
        normal_retirement_rows = normal_retirement_pages = max_objects = 0
        high_water_rows = 0
        reordering = selected_reordering = projected_saved_gets = projected_saved_callbacks = 0
        for row in rows:
            op = row['operation']
            g = op['generation']
            delta = row['cloudflare_workload_delta']
            xs = chunks(current)
            rehydrates = int(row['service_metrics_delta'].get('cloud_view_rehydrates', 0)) if cold else 0
            assert delta['r2_class_b'] == rehydrates * len(xs), (name, trial['sample'], op['key'])
            assert delta['r2_read_bytes'] == rehydrates * sum(xs), (name, trial['sample'], op['key'])
            full_gap = greedy(xs) - optimal(tuple(xs))
            assert full_gap >= 0
            reordering += rehydrates * full_gap
            selected = current
            if cold and op['group'] in ['history_no_haves', 'history_with_haves']:
                cutoff = max(generations[x] for x in [op['want'], op['have']] if x is not None)
                selected = [p for p in current if p['floor'] <= cutoff]
            ys = chunks(selected)
            selected_reordering += rehydrates * (greedy(ys) - optimal(tuple(ys)))
            projected_saved_gets += rehydrates * (len(xs) - len(ys))
            projected_saved_callbacks += rehydrates * (greedy(xs) - greedy(ys))
            if op['kind'] == 'push':
                puts = delta['r2_class_a']
                deletes = delta['r2_deletes']
                max_objects = max(max_objects, len(xs) + puts)
                high_water_rows += delta['do_rows_written'] - puts - deletes - 1
                if row['compaction_delta']['count']:
                    chosen = [p for p in current if p['level'] == 0 and p['created'] != 1]
                    assert len(chosen) == deletes == 3
                    assert g in [5, 9, 13, 17, 21, 25, 29]
                    floor = min([p['floor'] for p in chosen] + [g])
                    current = [p for p in current if p not in chosen]
                    normal_retirement_rows += deletes
                    normal_retirement_pages += 1
                    level = 1
                else:
                    assert deletes == 0
                    floor = g
                    level = 0
                current.append({'bytes': delta['r2_written_bytes'], 'created': g, 'floor': floor, 'level': level})
            all_checks += 1
        run = next(r for r in doc['cloudflare_runs'] if r['sample'] == trial['sample'] and not r['warmup'])
        final = run['final_object_stats']
        assert [p['bytes'] for p in current] == [p['pack_bytes'] + p['index_bytes'] for p in final['manifest']['packs']]
        samples.append({
            'sample': trial['sample'], 'fixed': trial['fixed_workload'],
            'inferred_normal_peak_writes': high_water_rows,
            'max_catalog_entries_before_retirement': max_objects,
            'final_catalog_entries': final['cloudflare_services']['r2_live_objects'],
            'sqlite_bytes': final['cloudflare_services']['do_sqlite_bytes'],
            'normal_retirement_normalized_rows': normal_retirement_rows,
            'normal_retirement_packed_page_rows': normal_retirement_pages,
            'gross_catalog_page_rows_saved': normal_retirement_rows - normal_retirement_pages,
            'actual_read_rows_reproduced': len(rows),
            'whole_layout_bulk_reordering_callbacks_saved': reordering,
            'projected_R10_layout_bulk_reordering_callbacks_saved': selected_reordering,
            'projected_R10_gets_saved': projected_saved_gets,
            'projected_R10_callbacks_saved': projected_saved_callbacks,
        })
    result['profiles'][name] = {'source_report_sha256': hashlib.sha256(report_path.read_bytes()).hexdigest(), 'samples': samples}
result['resource_monthly_usd_mean'] = dict(resources)
assert abs(sum(resources.values()) - result['fixed_known_usd']) < 1e-12
result['matched_read_rows'] = all_checks
result['catalog_pages_gross_rows_saved_per_four_profile_replay'] = sum(p['samples'][0]['gross_catalog_page_rows_saved'] for p in result['profiles'].values())
result['catalog_pages_gross_saving_usd_at_1000_replays'] = result['catalog_pages_gross_rows_saved_per_four_profile_replay'] * .001
result['conditional_R9_R10_R11_denominator_before_added_costs_usd'] = result['fixed_known_usd'] - .0576 - .05643 - .060
result['catalog_pages_gross_fraction_against_conditional_denominator'] = result['catalog_pages_gross_saving_usd_at_1000_replays'] / result['conditional_R9_R10_R11_denominator_before_added_costs_usd']
target = pathlib.Path('/tmp/git-zero-remaining-cost-audit-data.json')
target.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: v for k, v in result.items() if k != 'profiles'}, indent=2))
for name, p in result['profiles'].items():
    print(name, [{k: v for k, v in s.items() if k != 'fixed'} for s in p['samples']])
