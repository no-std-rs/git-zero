#!/usr/bin/env python3
"""Read sealed Git blobs/reports; write only this audit's offline evidence JSON."""
import collections
import datetime
import hashlib
import json
import pathlib
import subprocess
from decimal import Decimal as D

OUT = pathlib.Path(__file__).resolve().parent
REPO = pathlib.Path('/tmp/git-zero-hosting-cost-loop')
BASE = pathlib.Path('/tmp/git-zero-hosting-loop-artifacts/interleaved-v1/ci/r19-r20-screen/run-39128927970')
R20 = 'b74c91738a3fbfe3ebf30a936fbf9d7ff79f1ae0'
R19 = 'f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5'
FIT = '5b523c9c9b8d8b8876281371c888d5af77757df2'
PRIMARY = 'd5cdefe53fc24f34114292b7d19563e73afea293'
WORKER = '7f41db4407963ed04e6cbe19556b1416633caf18'
MIB = 1 << 20
BULK = 16 * MIB
REPEATS = 1000
PROFILES = ['16mib-cold', '16mib-warm', '64mib-cold', '64mib-warm']
FIELDS = ['worker_requests', 'worker_gateway_requests', 'worker_service_requests',
          'worker_observer_requests', 'do_requests', 'do_rows_read', 'do_rows_written',
          'r2_class_a', 'r2_class_b', 'r2_deletes', 'r2_read_bytes', 'r2_written_bytes']
RATES = {'r2_class_a': D('4.50') / 1000000,
         'r2_class_b': D('0.36') / 1000000,
         'worker_requests': D('0.30') / 1000000,
         'do_requests': D('0.15') / 1000000,
         'do_rows_read': D('0.001') / 1000000,
         'do_rows_written': D('1') / 1000000}
INPUTS = {}
CHECKS = collections.Counter()

def digest(data):
    return hashlib.sha256(data).hexdigest()

def read(path):
    path = pathlib.Path(path)
    data = path.read_bytes()
    INPUTS[str(path)] = {'bytes': len(data), 'sha256': digest(data)}
    return data

def git(*args):
    return subprocess.check_output(['git', '-C', str(REPO), *args])

def require(condition, name):
    assert condition, name
    CHECKS[name] += 1

def opdelta(row, op, field='attempts'):
    return (row['cloudflare_services_after']['operations'].get(op, {}).get(field, 0)
            - row['cloudflare_services_before']['operations'].get(op, {}).get(field, 0))

def key_count(row, which, key):
    return next((x['get_attempts'] for x in row['cloudflare_services_' + which]
                 ['r2_key_access']['top_keys'] if x['key'] == key), None)

def callback_lower(byte_count, prefetched):
    return max(0, (byte_count + BULK - 1) // BULK - prefetched)

source_inventory = []
sources = {}
paths = [
    'src/native.rs', 'src/lib.rs', 'src/storage.rs', 'src/cloudflare.rs',
    'src/first_read.rs', 'src/socket_rpc.rs', 'src/cost.rs', 'src/accounting.rs',
    'src/http.rs', 'src/benchmark_cost.rs', 'src/native/history.rs',
    'cloudflare/src/index.js', 'cloudflare/src/first-read.js',
    'cloudflare/src/service-socket.js', 'cloudflare/src/retirement-proof.js',
    'cloudflare/src/inline-catalog.js', 'cloudflare/src/catalog-pages.js',
]
for short in paths:
    path = 'examples/repository-host/' + short
    data = git('show', R20 + ':' + path)
    sources[short] = data.decode()
    source_inventory.append({'commit': R20, 'path': path,
        'git_blob': git('rev-parse', R20 + ':' + path).decode().strip(),
        'bytes': len(data), 'sha256': digest(data),
        'frozen_primary_blob_equal': data == git('show', PRIMARY + ':' + path)})
require(git('rev-parse', R20 + '^').decode().strip() == R19, 'R20_exact_parent')
require(git('rev-parse', FIT + '^').decode().strip() == R20, 'fit_exact_parent')
for commit in [R19, R20, FIT, PRIMARY]:
    require(git('rev-parse', commit + ':examples/repository-host/cloudflare/src')
            .decode().strip() == WORKER, 'common_six_module_worker_tree')
require('pub const PRICING_VERIFIED_DATE: &str = "2026-10-17";' in sources['src/cost.rs'],
        'frozen_pricing_date')
for literal in ['4.50 / MILLION', '0.36 / MILLION', '0.30 / MILLION',
                '0.15 / MILLION', '0.001 / MILLION', '1.0 / MILLION', '0.015,']:
    require(literal in sources['src/cost.rs'], 'frozen_rate_literal')

scores = {p: json.loads(read(BASE / 'scores' / (p + '-score.json')))
          for p in ['r19', 'r20']}
comparison = json.loads(read(BASE / 'scores/r20-vs-r19.json'))
for name in ['SHA256SUMS', 'provenance.txt', 'providers.tsv']:
    read(BASE / 'scores' / name)
context = [
    '/tmp/git-zero-after-joint-residual-audit.md',
    '/tmp/git-zero-fit-response-independent-design-review.md',
    '/tmp/git-zero-fresh-bootstrap-implementation-prep.md',
    '/tmp/git-zero-r19-r20-fresh-root-guard-review.json',
    '/tmp/git-zero-r19-r20-fresh-root-service-scope-review.json',
    '/tmp/git-zero-fit-response-independent-review/REVIEW.md',
    str(REPO / 'docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r20-joint-chunk-candidate/source-review.md'),
]
for path in context:
    read(path)

priced = []
grouping_trials = []
grouping_rows = []
layout = []
oracle_cuts = []
do_epoch_changes = []
for profile in PROFILES:
    for provider in ['r19', 'r20']:
        path = BASE / 'profiles' / profile / provider / (profile + '.json')
        report = json.loads(read(path))
        require(report['complete'], 'complete_report')
        require(report['config']['samples'] == 2, 'two_priced_samples')
        require(report['config']['cloudflare_local']['monthly_workload_repetitions'] == REPEATS,
                'unchanged_1000_repetitions')
        require(report['config']['cloudflare_local']['worker_source']['sha256']
                == digest(sources['cloudflare/src/index.js'].encode()), 'report_worker_source_pin')
        runs = {(r['sample'], r['warmup']): r for r in report['cloudflare_runs']}
        for sample, warmup in [(-1, True), (0, False), (1, False)]:
            rows = [r for r in report['raw'] if r['backend'] == 'cloudflare-local'
                    and r['sample'] == sample and r['warmup'] == warmup]
            require(len(rows) == 88, 'exact_88_rows')
            require(sum(r['operation']['kind'] == 'push' for r in rows) == 32, 'exact_32_pushes')
            require(sum(r['operation']['kind'] == 'fetch' for r in rows) == 56, 'exact_56_fetches')
            totals = collections.Counter({f: 0 for f in FIELDS})
            totals_grouping = collections.Counter()
            for row in rows:
                usage = row['cloudflare_workload_delta']
                before, after = row['cloudflare_services_before'], row['cloudflare_services_after']
                require(before['accounting_complete'] and after['accounting_complete'], 'complete_row_snapshots')
                require(before['counter_epoch'] == after['counter_epoch'], 'same_worker_counter_epoch')
                if before['do_instance_epoch'] != after['do_instance_epoch']:
                    do_epoch_changes.append({'provider': provider, 'profile': profile, 'sample': sample,
                        'warmup': warmup, 'operation': row['workload'],
                        'before': before['do_instance_epoch'], 'after': after['do_instance_epoch']})
                for f in FIELDS:
                    actual = sum(after['by_phase'].get(p, {}).get(f, 0)
                                 - before['by_phase'].get(p, {}).get(f, 0)
                                 for p in ['push', 'fetch'])
                    require(actual == usage[f], 'row_fixed_phase_closure')
                    totals[f] += usage[f]
                require(usage['worker_gateway_requests'] == 2, 'two_actual_gateway_requests')
                require(usage['worker_observer_requests'] == 0, 'no_objective_observer_credit')
                require(usage['worker_requests'] == usage['worker_gateway_requests']
                        + usage['worker_service_requests'], 'actual_worker_categories')
                require(usage['worker_cpu_ms'] is None and usage['do_gb_seconds'] is None,
                        'compute_stays_unknown')
                require(opdelta(row, 'manifest_get') == 2, 'two_actual_fresh_authorities')
                require(opdelta(row, 'r2_put') == usage['r2_class_a'], 'write_RPC_equals_actual_puts')
                require(opdelta(row, 'r2_get') == usage['r2_class_b'], 'ClassB_equals_actual_GETs')
                require(opdelta(row, 'r2_get', 'read_bytes') == usage['r2_read_bytes'], 'actual_GET_bytes')
                for op in ['manifest_get', 'manifest_cas', 'r2_get', 'r2_put', 'r2_delete']:
                    require(opdelta(row, op, 'failures') == 0, 'no_hidden_failed_binding_attempt')
                if provider != 'r20':
                    continue
                metrics = row['service_metrics_delta']
                for f in ['gateway_read_paid_discards', 'gateway_read_errors', 'gateway_read_generation_changed']:
                    require(metrics[f] == 0, 'no_extra_first_read_attempts_or_discard')
                prefetch = int(metrics['gateway_read_consumed'] + metrics['gateway_receive_consumed'])
                actual_callbacks = usage['do_requests'] - 2 - usage['r2_class_a']
                socket_rpcs = int(metrics['native_socket_rpc']['rpcs_completed'])
                require(socket_rpcs == usage['r2_class_a'] + actual_callbacks,
                        'socket_RPC_classification_independent_crosscheck')
                for f in ['failures', 'unsupported_upgrades']:
                    require(metrics['native_socket_rpc'][f] == 0, 'no_socket_failure_or_fallback')
                lower = callback_lower(usage['r2_read_bytes'], prefetch)
                require(actual_callbacks == lower, 'unchanged_bytes_callback_lower_bound_attained')
                evidence = {'profile': profile, 'sample': sample, 'warmup': warmup,
                    'operation': row['workload'], 'GETs': usage['r2_class_b'],
                    'GET_bytes': usage['r2_read_bytes'], 'actual_DO': usage['do_requests'],
                    'fresh_authority_DO': 2, 'write_RPCs': usage['r2_class_a'],
                    'gateway_first_batches': prefetch, 'actual_read_callbacks': actual_callbacks,
                    'volume_lower_bound_read_callbacks': lower, 'unfilled_callback_saving': actual_callbacks - lower}
                grouping_rows.append(evidence)
                totals_grouping.update({k: evidence[k] for k in [
                    'GETs', 'GET_bytes', 'actual_DO', 'fresh_authority_DO', 'write_RPCs',
                    'gateway_first_batches', 'actual_read_callbacks', 'volume_lower_bound_read_callbacks',
                    'unfilled_callback_saving']})
            run = runs[(sample, warmup)]
            if provider == 'r20':
                grouping_trials.append({'profile': profile, 'sample': sample, 'warmup': warmup,
                    'actual_fixed': dict(totals), 'grouping': dict(totals_grouping),
                    'whole_final_Native_socket_counter_not_fixed': run['final_object_stats']['native_socket_rpc']})
            if not warmup:
                peak = run['estimated_service_costs']['r2_observed_peak_bytes']
                resource = {f: D(totals[f]) * REPEATS * rate for f, rate in RATES.items()}
                resource['r2_storage'] = D(peak) / D(1000000000) * D('0.015')
                cost = sum(resource.values())
                score_trial = next(x for x in scores[provider]['profiles'][profile]['trials'] if x['sample'] == sample)
                require(dict(totals) == {k: score_trial['fixed_workload'][k] for k in FIELDS}, 'independent_fixed_totals_match_score')
                require(abs(cost - D(str(score_trial['known_monthly_fixed_workload_usd']))) < D('0.0000000000001'), 'manual_pricing_matches_trial')
                priced.append({'provider': provider, 'profile': profile, 'sample': sample,
                    'fixed': dict(totals), 'observed_peak_bytes_once_30d': peak,
                    'resource_monthly_usd': {k: str(v) for k, v in resource.items()}, 'known_usd': str(cost)})
            if provider != 'r20' or warmup:
                continue
            push_rows = [r for r in rows if r['operation']['kind'] == 'push']
            write_layout = []
            for row in push_rows:
                u = row['cloudflare_workload_delta']
                require(u['r2_class_a'] == (u['r2_written_bytes'] + BULK - 1) // BULK,
                        'per_push_current_bytes_16MiB_PUT_count_attained')
                write_layout.append({'operation': row['workload'], 'actual_PUTs': u['r2_class_a'],
                    'actual_written_bytes': u['r2_written_bytes'],
                    'ceil_current_written_bytes_over_16MiB': (u['r2_written_bytes'] + BULK - 1) // BULK})
            final_manifest = run['final_object_stats']['manifest']
            secondary = [p for p in final_manifest['packs'] if p['level'] == 1]
            minimum_secondary = min(p['pack_bytes'] + p['index_bytes'] for p in secondary)
            final_keys = rows[-1]['cloudflare_services_after']['r2_key_access']
            layout.append({'profile': profile, 'sample': sample, 'pushes': write_layout,
                'fixed_written_bytes': totals['r2_written_bytes'],
                'one_percent_extra_written_bytes': str(D(totals['r2_written_bytes']) / 100),
                'minimum_live_level1_pack_bytes': minimum_secondary,
                'one_hot_cross_push_copy_write_amplification_fraction': str(D(minimum_secondary) / D(totals['r2_written_bytes'])),
                'final_manifest_pack_chunk_lengths': [
                    {'id': p['id'], 'level': p['level'], 'pack_bytes': p['pack_bytes'],
                     'index_bytes': p['index_bytes'], 'bundle_lengths': [c['len'] for c in p['bundle']]}
                    for p in final_manifest['packs']],
                'top_key_summary_limit': {'displayed': len(final_keys['top_keys']),
                    'tracked': final_keys['tracked_keys'], 'truncated_flag': final_keys['truncated'],
                    'meaning': 'False capacity truncation does not make the top10 display an exhaustive per-key timeline.'}})
            if profile != '64mib-cold' or sample != 0:
                continue
            root_chunks = final_manifest['packs'][0]['bundle']
            require([x['len'] for x in root_chunks] == [BULK, BULK, 653384], 'actual_64_anchor_partition')
            anchor_key, tail_key = root_chunks[1]['key'], root_chunks[2]['key']
            anchor_gets = 0
            for row in rows:
                old, new = key_count(row, 'before', anchor_key), key_count(row, 'after', anchor_key)
                # This hot key stays displayed after the first PUT; prove its continuity.
                require(old is not None or row['workload'] == 'push_01', 'hot_anchor_key_present_before')
                require(new is not None, 'hot_anchor_key_present_after')
                anchor_gets += new - (old or 0)
            require(anchor_gets == 80, 'actual_80_hot_anchor_GETs')
            tail = next(x for x in final_keys['top_keys'] if x['key'] == tail_key)
            require(tail['get_attempts'] == 1 and tail['failures'] == 0, 'tail_exactly_one_GET_in_final_counter')
            for dm in [D('0'), D('0.25'), D('1'), D('2.5'), D('5'), D('10'), D('15')]:
                delta = int(dm * MIB)
                ideal_callbacks = 0
                for row in rows:
                    if row['workload'] == 'push_01':
                        ac = 0
                    else:
                        ac = key_count(row, 'after', anchor_key) - key_count(row, 'before', anchor_key)
                    # The sole actual tail read is in the first latest fetch; this is a stated conditional replay.
                    tc = int(row['workload'] == 'latest_fetch_01')
                    nbytes = row['cloudflare_workload_delta']['r2_read_bytes'] + delta * (tc - ac)
                    prefetch = int(row['service_metrics_delta']['gateway_read_consumed']
                                   + row['service_metrics_delta']['gateway_receive_consumed'])
                    ideal_callbacks += callback_lower(nbytes, prefetch)
                removed = 75 - ideal_callbacks
                oracle_cuts.append({'sample': sample, 'delta_MiB': str(dm),
                    'partition_lengths': [BULK, BULK - delta, 653384 + delta],
                    'assumption': 'Current hit/miss ownership and GET multiplicities retained; ideal repacking into16MiB batches; NO cache-feedback claim.',
                    'ideal_read_callbacks': ideal_callbacks, 'conditional_removed_callbacks': removed,
                    'DO_only_gross_credit_usd': str(D(removed) * REPEATS * RATES['do_requests'])})
            # All per-row bytes weakly decrease as delta grows, so the maximal allowed delta is an upper bound in this conditional model.
            delta = BULK - 653384
            ideal_callbacks = 0
            for row in rows:
                ac = 0 if row['workload'] == 'push_01' else key_count(row, 'after', anchor_key) - key_count(row, 'before', anchor_key)
                tc = int(row['workload'] == 'latest_fetch_01')
                nbytes = row['cloudflare_workload_delta']['r2_read_bytes'] + delta * (tc - ac)
                require(nbytes >= 0 and ac >= tc, 'conditional_cut_bytes_monotone')
                prefetch = int(row['service_metrics_delta']['gateway_read_consumed'] + row['service_metrics_delta']['gateway_receive_consumed'])
                ideal_callbacks += callback_lower(nbytes, prefetch)
            oracle_cuts.append({'sample': sample, 'delta_bytes': delta, 'maximal_16MiB_tail': True,
                'partition_lengths': [BULK, 653384, BULK], 'conditional_removed_callbacks': 75 - ideal_callbacks,
                'DO_only_gross_credit_usd': str(D(75 - ideal_callbacks) * REPEATS * RATES['do_requests']),
                'deliberately_loose_all7_actual_64cold_Worker_service_requests_credit_usd': '0.002100',
                'conditional_gross_ceiling_usd_including_loose_Worker_credit': str(D(75 - ideal_callbacks) * REPEATS * RATES['do_requests'] + D('0.0021')),
                'invalidating_case': 'Real source can admit the shortened second chunk and reject the enlarged tail, changing signed descriptor mask/residency and later GETs. This is not an upper bound over those feedback effects or all layouts.'})

objective = {}
for provider in ['r19', 'r20']:
    by_sample = [sum(D(t['known_usd']) for t in priced if t['provider'] == provider and t['sample'] == s)
                 for s in [0, 1]]
    require(abs(sum(by_sample) / 2 - D(str(scores[provider]['known_monthly_fixed_workload_usd']))) < D('0.0000000000001'),
            'manual_known_objective_matches_score')
    objective[provider] = str(sum(by_sample) / 2)
    objective[provider + '_trial_min_usd'] = str(min(by_sample))
    objective[provider + '_trial_max_usd'] = str(max(by_sample))
objective['R20_one_percent_known_usd'] = str(D(objective['r20']) / 100)
objective['R19_to_R20_reduction_fraction'] = str(1 - D(objective['r20']) / D(objective['r19']))

result = {
    'audit': 'actual R20 next optimization, bounded callbacks and physical layout',
    'sealed_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'status': 'NO_NEW_QUALIFIED_CANDIDATE; bounded grouping rejected by actual per-row lower bound; layout conclusions scoped below',
    'authorization': 'Read/offline audit only. No production edit/build/runtime/Git object generation/push/CI/deployment/delegation. STOP after handoff.',
    'source_commits': {'R19': R19, 'R20': R20, 'fit_source': FIT, 'frozen_primary': PRIMARY, 'common_worker_tree': WORKER},
    'source_inventory': source_inventory,
    'input_inventory': INPUTS,
    'checks': dict(CHECKS),
    'offline_verification_attempts': ['Initial offline pass stopped at an overly strict same-DO-instance-epoch assertion. Real DO instance changes are permitted and must retain receipt accounting. The final verifier inventories those actual changes instead, then passes all phase/API/callback/pricing checks. No report/source/runtime was altered.'],
    'actual_DO_epoch_changes': do_epoch_changes,
    'epoch_accounting_note': 'DO instance changes are retained, not treated as failure or zeroed counters. Authoritative Worker receipt totals remain complete. Instance-local top-key summaries can reset; the conditional anchor arithmetic uses only sample0, whose64cold instance is continuous.',
    'pricing': {'verified_date': '2026-10-17', 'repetitions_each_profile': REPEATS,
        'rates_per_one_actual_operation_usd': {k: str(v) for k, v in RATES.items()},
        'R2_storage_per_decimal_GB_month': '0.015', 'observed_peak_retained_once_30d': True,
        'source': 'examples/repository-host/src/cost.rs @ ' + R20},
    'objective_independent_reconstruction': objective,
    'priced_trials': priced,
    'grouping_trials_including_warmups': grouping_trials,
    'grouping_row_proof_including_warmups': grouping_rows,
    'grouping_result': {
        'priced_rows': 704, 'warmup_rows_separate': 352,
        'one_repetition_all4': {'actual_DO_requests': 917, 'fresh_authority_DO': 704,
            'write_RPCs': 132, 'read_callbacks': 81, 'gateway_first_batches': 111,
            'GETs': 314, 'minimum_unfilled_read_callback_saving': 0},
        'known_gross_credit_under_unchanged_bytes_authority_masks_and16MiB_bound_usd': '0',
        'full_bill': None,
        'proof_scope': 'Each existing application response carries <=16MiB payload. With authority/write RPCs fixed, C>=max(0,ceil(B/16MiB)-P). Current C attains even this optimistic bound for every row; exact ordering could only raise it.',
        'falsification': 'Offline: one priced row with actual_C>max(0,ceil(actual_GET_bytes/16MiB)-successful_first_batches) reopens the question. None exists. Any future representative trace should first repeat this cheap arithmetic; do not implement merely from scalar GET count.',
        'extra_costs': 'There are zero saved callbacks to pay for allocation/headers/probes/capability/RPC changes. Reordering or concurrency changes failure ordering but cannot lower these counts. Every actual attempt stays charged; no retry/fallback credit.'},
    'physical_layout_evidence': layout,
    'conditional_anchor_cut_arithmetic': oracle_cuts,
    'larger_physical_container_with_logical_range_chunks': {
        'classification': 'Separate conditional future FORMAT/service-protocol design. Not supported now; not a qualified implementation candidate under unchanged16MiB receive limits.',
        'actual_initial64_bytes': 34207816,
        'actual_initial64_objects_each_cold_warm': 3,
        'conditional_target_initial64_objects_each_cold_warm': 1,
        'conditional_saved_PUTs_all4_per_repetition': 4,
        'ClassA_only_gross_ceiling_usd': '0.018000',
        'percent_R20_known_objective': str(D('0.018') / D(objective['r20']) * 100),
        'headroom_after1percent_threshold_before_all_extra_known_costs_usd': str(D('0.018') - D(objective['r20']) / 100),
        'gross_assumptions': 'Each64 first snapshot becomes exactly one physical object with identical34207816payload bytes; all32 pushes stay durable; four actual ClassA PUTs truly disappear with no new billable upload steps, extra manifests, fallback, recovery, mutation, SQL, read or storage cost. No ClassB/DO/Worker credit granted.',
        'current_source_blockers': [
            'Chunk only serializes key+len; the chunks/SHA256 key authenticates the complete returned payload, with no container key, offset or separate logical digest.',
            'Worker readBulk calls BLOBS.get(physicalKey) without range; its payload/expected length refers to the full object. Native verification likewise hashes the entire logical payload against the descriptor key.',
            'PendingChunk::new, staged publication, Worker standalone blob PUT and coupled upload all bound one new payload to16MiB. Socket fragments do not relax the complete application message bound.',
            'Inline catalog, live/peak accounting and retirement/GC operate on actual immutable object keys and sizes. Shared container slices need reference-safe physical liveness and truthful pre-delete peak ownership.'
        ],
        'separate_design_requirements': [
            'Keep logical content digest/order/membership and exact masks/token semantics; authenticated bounded mapping binds immutable physical key+total size+offset+logical length/digest without alias/overlap ambiguity. Mapping ownership belongs inside existing limits.',
            'Exact logical range responses must verify returned range and length plus logical SHA before cache admission/files; complete selected-body bytes, actual R2 attempted/consumed bytes and receipts stay measured. No fetching34MiB to return/cache16MiB.',
            'Supply an actual durable container upload protocol compatible with unchanged complete16MiB receive bound. A new multipart/streaming mechanism must enumerate and price every billable create/part/complete/abort/callback and uncertainty; no single-PUT saving is assumed.',
            'Prove legacy read/reopen compatibility, incomplete/orphan uploads, stale/lost committed CAS, shared-slice retirement, sync-before-delete, same-Native recovery/GC and restart without replay of ambiguous writes.'
        ],
        'first_falsification': 'Before code or runtime, specify the exact upload message/physical-operation sequence and count billable ClassA operations for34207816B under the16MiB complete-receive cap. If it cannot beat3 actual initial PUTs per64 profile, this4-PUT hypothesis is false. Then price every mapping/SQL/callback/range/retirement delta against the$0.0058752856382 headroom and prove each profile<=1% read/write/live/peak/transfer amplification. Only after independent format review could a Rust one-first-push/range/read/restart/fault gate be considered.',
        'net_lower_bound_usd': None,
        'full_bill_usd': None,
        'no_GO': True,
    },
    'layout_result': {
        'within_same_successful_push_current_bytes': 'Every one of256 priced R20 pushes already uses ceil(current_written_bytes/16MiB) PUTs. This is a current-format/layout statement only, not a global durable-write/CAS floor.',
        'hot_cross_push_coalescing': 'Reject straightforward concatenate-and-republish of any existing4.37/4.39MiB level1 object: one copy alone adds10.91%/6.62% fixed written bytes. A header/offset representation also needs new verified slicing/storage semantics and real retirement/fault proof.',
        'small_pack_only_coalescing': 'No >=1% net opportunity established. Separate initial new bytes from avoided repeat GETs; charge COW bytes, storage peaks, subset read amplification, manifest/control ownership and lost cache hits. top10 summaries are incomplete.',
        'anchor_repartition': 'Not qualified; current16/16/653384 split shows80 hot second-chunk GETs, but changing boundary changes first-fit residency. Conditional same-hit trace arithmetic is below1% even with free use of all7 actual Worker service requests; it is not a bound over changed masks/cache feedback.',
        'cheap_falsification_before_runtime': 'For any exact proposed layout, independently reconstruct current per-row state first, then replay only physical boundaries with actual capacities/control pressure and current source admission/retention. Reject if any profile read/write/live/peak/client-transfer/HTTP-response amplification exceeds1%, or conservative priced net saving is<1%. If credible, one Rust-only ordinary16/64 cold/warm same-source pair plus exact authority/token/fault/retirement/restart controls precedes any full screen. No test was run or authorized here.'},
    'source_refs': [
        {'path': 'src/lib.rs', 'lines': '348-390', 'finding': 'Collect all selected missing descriptors once; one bulk adapter call across packs, after exact prefetched prefix.'},
        {'path': 'src/cloudflare.rs', 'lines': '319-349;983-1022', 'finding': 'Existing bounded longest-prefix16MiB bulk grouping, complete request validation and per-batch verification; legacy scalar only if no eligible bounded batch.'},
        {'path': 'src/native.rs', 'lines': '2012-2030;2162-2218', 'finding': 'First batch already from same current missing list; authenticate and verify before mask/authority continuation; paid discard retained.'},
        {'path': 'cloudflare/src/index.js', 'lines': '347-362;1388-1435;1438-1466', 'finding': 'One DO dispatch per bridge; per-object ordered R2 gets remain separately metered inside existing batch; authority+first batch fused.'},
        {'path': 'cloudflare/src/service-socket.js', 'lines': '12-13;28-32;84-116', 'finding': '16 RPC rotation; one complete bounded message maps to one existing DO RPC; no pipeline/ACK bypass.'},
        {'path': 'src/socket_rpc.rs', 'lines': '21-35;134-197', 'finding': '262144B joint reservation and no replay after ambiguous dispatch; smaller RFC pieces are not new DO requests.'},
        {'path': 'src/native.rs', 'lines': '672;694;1886-1925', 'finding': 'Native physical chunk16MiB; owning-budget first-fit admission can change residency after any boundary shift.'},
        {'path': 'src/lib.rs', 'lines': '118-158', 'finding': 'Already concatenates physical pack+index into16MiB chunks; private tail is coupled to publication.'},
        {'path': 'src/storage.rs', 'lines': '110-129;196-201;247-258', 'finding': 'Pending payload16MiB; descriptor key+len only; complete logical payload hash/length verification, no physical range mapping.'},
        {'path': 'cloudflare/src/index.js', 'lines': '212-235;1407-1421', 'finding': 'Exact upload fields and16MiB chunk; full-object GET, no range request; new container/range descriptors are unsupported.'},
        {'path': 'cloudflare/src/index.js', 'lines': '14;1388-1403;1479-1485', 'finding': 'Current new-blob receipt and bulk payload bounds16MiB; larger objects/stream-of-responses would alter audited service limits.'},
        {'path': 'src/cost.rs', 'lines': '290-356', 'finding': 'Frozen linear request/SQL/storage prices; no GET-byte price or CPU/duration inference.'},
    ],
    'ranked_handoff': [
        'Existing fit-response exact source remains pending its own independent controls; no approval or additive gain granted by this audit.',
        'Existing deferred bootstrap remains separate and conditional; reprice against selected measured predecessor, charge all extra startup GC reads.',
        'No new >=1% candidate promoted from grouping/layout. Larger physical containers with logical range chunks are a separate conditional format/service design and need a concrete bounded upload cost proof first; alternatives are not exhausted.'
    ],
    'unknown_dimensions': ['Worker CPU', 'DO duration', 'monthly SQLite storage', 'Native compute'],
    'total_estimate_usd': None,
    'screen_context': {'decision': comparison['comparison']['decision'],
        'eligible_rejected_candidate_for_plateau': comparison['comparison']['eligible_rejected_candidate_for_plateau'],
        'five_p95_groups_failed': True, 'five_original_R1_final_pairs_per_profile': 'NOT DONE'},
    'no_claims': ['No winner/promotion', 'No speed improvement', 'No full bill', 'No global request/write/storage floor',
                  'No optimum/plateau/stop conclusion', 'No implementation or runtime GO'],
    'verification_program': {'path': str(pathlib.Path(__file__).resolve()),
        'bytes': pathlib.Path(__file__).stat().st_size, 'sha256': digest(pathlib.Path(__file__).read_bytes())},
}
(OUT / 'AUDIT.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'checks': dict(CHECKS), 'objective': objective,
    'priced_R20_callback_profiles': [x for x in grouping_trials if not x['warmup'] and x['sample'] == 0],
    'anchor_cut_maximal': [x for x in oracle_cuts if x.get('maximal_16MiB_tail')],
    'json_bytes': (OUT / 'AUDIT.json').stat().st_size,
    'json_sha256': digest((OUT / 'AUDIT.json').read_bytes())}, indent=2))
