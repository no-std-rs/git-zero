#!/usr/bin/env python3
import datetime
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import os

OUT=Path(__file__).parent
PUB=Path('/tmp/git-zero-hosting-fit-response/docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r21-fit-response-candidate')
audit=json.loads((OUT/'publication-audit.json').read_text())
reconstruction=json.loads((OUT/'reconstruction-audit.json').read_text())
inventory=json.loads((OUT/'INPUT_SHA256_INVENTORY.json').read_text())
inputs={x['path']:x for x in inventory['read_inputs']}
def read(p):
 p=Path(p);b=p.read_bytes();inputs[str(p)]={'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()};return b
def load(name):
 b=read(PUB/name);return json.loads(gzip.decompress(b) if name.endswith('.gz') else b)
known=load('known-scope-nulls.json')
semantic=load('reviews/final/completion-semantic-audit.json.gz')
first=load('reviews/final/completion-semantic-audit-first-exact-float-comparison.json.gz')
final=load('reviews/final/REVIEW.json.gz')
fault=load('evidence/fault-coverage-final.json.gz')
ulp=semantic['observed_one_ulp_timing_advisory_cost_roundtrips']
assert known['comparison_scope']['paths']==[{'scope':x['scope'],'path':x['path']} for x in ulp]
assert len(ulp)==7 and len({(x['raw_parsed_value'],x['enclosing_parsed_value']) for x in ulp})==3
assert len(first['errors'])==4 and not semantic['errors']
assert len([x for x in fault['same_Native_followups'] if x['physical_GC_before_first_PUT'] is True])==11
assert [x['mode'] for x in fault['same_Native_followups'] if x['ordering_excluded']]==['unknown-sql','gateway-lost-reply']
assert all(x['physical_GC_before_first_PUT'] is None for x in fault['same_Native_followups'] if x['ordering_excluded'])
native='2941f19a4f25353b86f1ae3e8e75315480f17151ff9310561b828d0c8ad81369'
for v in ['v0','v1','v2']:
 m=load(f'source-maps/{v}-manifest.json.gz')
 assert m['files']['native-elf']['sha256']==native
 assert m['files']['native.rs']['sha256']=='ad04a572eadaec569fae885f781a4f3dd44e1fe9eaed363cbd16e54090894862'
 assert m['files']['cli.rs']['sha256']=='a7c3fc9c8f685ece6844f60c218e2edb9316b182674ba2888ec72f3f1162ef78'
copies=load('COPY-MAP.json')['files'];source_paths={x['source_path'] for x in copies}
for folder in ['/tmp/git-zero-fit-response-independent-review','/tmp/git-zero-fit-response-independent-review-supplement-01','/tmp/git-zero-fit-response-independent-final-review']:
 actual={str(p) for p in Path(folder).rglob('*') if p.is_file()}
 assert actual <= source_paths,(folder,actual-source_paths)
host_log=read(OUT/'reconstructed/controls/faults-fit-v2-02/host.log').decode()
assert host_log.splitlines()[16]=='statistics failed: service accounting is incomplete'
assert known['fault_final']['status']==503 and known['fault_final']['accounting_complete'] is False
assert all(known['fault_final'][k] is None for k in ['SQL_rows_read','SQL_rows_written','object_stats','priced_estimate'])
assert known['future']['mandatory_five_fresh_original_R1_final_all4']=='NOTDONE'
assert known['future']['fresh_matching_screen_pairs']=='NOTDONE'
assert all(x is None for x in known['unknown_cost_scope'].values())
assert final['runtime_tested_SOURCE']=='8287ef863e8ec78950ed3a9a59d509a7ed6e1dc2'
assert final['runtime_tested_runner_ELF']=='80df43e2f9c804f878b5f25d6e7cad8e68a191b897d95fc8cc50f3c7cc3e6660'
assert final['final_source_relinked_or_runtime_rerun'] is False
assert not audit['errors'] and reconstruction['status']=='PASS'
env=dict(os.environ,GIT_OPTIONAL_LOCKS='0',GIT_PAGER='cat')
repo='/tmp/git-zero-hosting-fit-response'
diff=subprocess.run(['git','-C',repo,'diff','--check',audit['parent'],audit['commit']],capture_output=True,env=env)
assert diff.returncode==0
(OUT/'independent-committed-diff-check.log').write_bytes(diff.stdout+diff.stderr)
assert subprocess.check_output(['git','-C',repo,'status','--porcelain=v1'],env=env)==b''
assert subprocess.check_output(['git','-C',repo,'rev-parse','HEAD'],env=env).decode().strip()==audit['commit']
read(OUT/'reconstruction.log');read(OUT/'independent-committed-diff-check.log')
inventory['read_inputs']=sorted(inputs.values(),key=lambda x:x['path'])
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps(inventory,indent=2)+'\n')
(OUT/'INPUT_SHA256SUMS').write_text(''.join(x['sha256']+'  '+x['path']+'\n' for x in inventory['read_inputs']))
verdict={
 'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
 'verdict':'LEDGERAPPROVED',
 'approved_docs_commit':audit['commit'],
 'exact_SOURCEAPPROVED_parent':audit['parent'],
 'approval_scope':'Exact R21 docs-only selected evidence publication; root separately controls integration and next screen.',
 'concrete_blockers':[],
 'ledger_gate_satisfied':True,
 'screen_priced_speed_cost_full_bill_promotion_plateau_integration_or_stopping_acceptance':False,
 'publication':{k:audit[k] for k in ['publication_regular_files','publication_stored_bytes','source_originals','source_original_bytes','actual_original_byte_comparisons','signed_raw_original_bindings_without_rehash','archive_members','archive_regular_files','archive_directory_headers','archive_control_originals','PACK_files','PACK_bytes','production_tree_paths']},
 'audit_check_count':len(audit['checks']),
 'all_audit_checks_pass':True,
 'all_stored_file_SHA_size_and_Git_blob_bindings':True,
 'all_decoded_original_SHA_size_and_source_bindings':True,
 'signed_raw_binding_authority':'Final SOURCE-review original input inventory SHA8102c427d8637aa5a4987c9df01712dc5d5ea47189a583c5d3fb43892f51daba; 946 exact source records. No host raw rehash in this ledger review.',
 'reconstruction':{k:v for k,v in reconstruction.items() if k!='files'},
 'original_review_directories_complete_and_byte_exact':True,
 'historical_failed_exact_float_comparison_errors':first['errors'],
 'seven_exact_one_ULP_exceptions':ulp,
 'receipt_rule':'Integer/string/bool/NULL exact; seven enumerated timing/advisory-cost paths over three underlying values only; unchanged original bytes; no broader float tolerance.',
 'fault_503_Native_reason':{'published_file':'selected-evidence.tar.gz::controls/faults-fit-v2-02/host.log','line':17,'text':'statistics failed: service accounting is incomplete','accounting_complete':False,'SQL_read':None,'SQL_write':None,'object_stats':None,'estimate':None},
 'fault_GC_ordering':{'true_checks':11,'excluded_unclaimed_NULL':['unknown-sql','gateway-lost-reply']},
 'preserved_historical_failures_and_gaps':final['historical_failures_and_gaps_preserved'],
 'preserved_source_and_runtime_distinction':{'final_SOURCE':audit['parent'],'tested_SOURCE':final['runtime_tested_SOURCE'],'tested_runner_ELF':final['runtime_tested_runner_ELF'],'unchanged_Native_ELF_all_versions':native,'final_source_relinked_or_runtime_rerun':False,'final_patch_SHA256':final['final_patch_SHA256']},
 'retained_ownership_vs_total_request_heap_scope_preserved':True,
 'startup_observer_final_receipt_order_and_original_metrics_preserved':'Exact original records and sealed final-source audit retained; ledger review did not rerun controls.',
 'payload_exclusions_verified':['ELF payload','SQLite payload','generated Git/R2/SQLite/repository/fixture state','large wire body','whole wanted archive','standalone seed pack','duplicate individual controls or redundant second archive'],
 'permitted_PACK_samples':{'files':82,'bodies':15,'requests':67,'bytes':41706,'minimum_bytes':323,'maximum_bytes':2766,'all_non_UTF8_exactly_declared':True,'decoded_object_inference':False},
 'author_publication_whitespace_failure_preserved':'Initial staged check failed on original literal blank-context whitespace. Exact gzip carriers and original failure log hashes verified. Final committed diff check passes.',
 'root_precheck_clarification':'947 regular files include 946 originals plus internal SHA256SUMS, and 13 safe directory headers give 960 total members. No archive change required.',
 'reviewer_initial_audit_correction':'First outer-checksum coverage comparison omitted normalization of harmless leading ./ names. Initial audit retained separately; normalized path comparison passes; publication unchanged.',
 'future':{'two_fresh_matching_screen_pairs':'NOTDONE','mandatory_five_fresh_original_R1_final_all_four_workloads':'NOTDONE'},
 'unknown_cost_scope':known['unknown_cost_scope'],
 'reviewer_actions':{'read_only_publication_Git_original_and_sealed_inventory_checks':True,'offline_helper_reconstruction':'Own fresh review directory only','source_edits_build_runtime_services_CI_push_main_primary_access_raw7GB_rehash_or_delegation':False,'write_directory':str(OUT)},
 'STOP':True,
}
(OUT/'REVIEW.json').write_text(json.dumps(verdict,indent=2)+'\n')
md='''# Independent R21 ledger/publication review

**LEDGERAPPROVED: `d573f463655083885a3cd51a76761c7967834777`, exact SOURCEAPPROVED parent `95fdf58f6bd7298736cfe124725e4f7e6c9aceb9`.** No concrete ledger blocker remains. Approval covers this exact docs-only publication. Root separately controls integration and the next screen. This provides no speed, priced-cost, full-bill, promotion, plateau, integration, or stopping acceptance. Two fresh matching screen pairs and the mandatory five fresh original-R1/final repetitions across all four workloads remain **NOTDONE**.

The exact committed file map, handoff MD/JSON, final independent SOURCE-review MD/JSON, and final input inventory match the supplied SHA-256 identities. All **116 files / 6,976,776 stored bytes** match their declared size, SHA-256, and Git blob. The actual working tree is clean at the reviewed commit. Its exact parent and b74→5b523→8287→95f→d573 ancestry match. Only the new R21 folder is added. Native `examples/repository-host/src` tree4717 and Worker `cloudflare/src` tree7f41 remain unchanged. Independent committed diff whitespace check passes.

The canonical archive is **753,056B**, SHA `9983f879ac58a988b0a2a3bc6459987f995ed0264d766f49a3b36af4381f16cd`. It contains **960 safe members: 947 regular files (946 selected control originals plus internal SHA256SUMS), and 13 directory headers**. Every path/type, unique member, exact member set, internal checksum entry, and decoded size/hash passes. Root's earlier draft count correction is valid; directory headers are allowed and the archive is unchanged. There are no duplicate individual control carriers or second redundant archive.

All **1,051 decoded originals / 29,775,842B** match COPY-MAP. The 105 accessible nonraw originals also compare byte-for-byte with their originals. Each of the 946 raw control originals binds exactly to the signed final SOURCE-review input inventory (SHA `8102c427d8637aa5a4987c9df01712dc5d5ea47189a583c5d3fb43892f51daba`). This ledger review deliberately did not repeat the already sealed 7GB host-raw hash audit. Full original review directories, design review, failed exact-comparison draft, historical maps/handoffs/logs, and audit helpers are present and byte-exact. No copied JSON was reserialized.

Positive payload inspection confirms exactly **82 declared PACK-v2 synthetic wire samples: 15 bodies, 67 requests, 41,706B total, 323–2,766B each**. Their actual signatures/offsets, hashes, sizes and encoding classifications match; they are exactly the 82 non-UTF8 originals. Every wire original is within64KiB and every source/text/metadata original within8MiB. No ELF or SQLite payload, generated Git/R2/SQLite/repository/fixture state, large wire body, whole wanted archive, or standalone seed pack is published. Signature observations imply no decoded object content. Inventories and hash maps describe excluded local raw state without publishing it.

The original portable verifier and reconstruction helper ran offline in this review's own fresh directory. Reconstruction restored all1,051 originals with exact hashes and sizes, plus COPY-MAP and the internal SHA256SUMS: **1,053 regular files**, no extras or symlinks. No service or build ran. The independent audit records8,792 passing checks. A first reviewer-only coverage comparison needed normalization of harmless leading `./` checksum names; that initial audit is retained separately, and the corrected comparison passes without changing the publication.

Published final SOURCE95f is clearly distinguished from actually tested SOURCE8287 and runner ELF80df43e2…. The final lint-only `.err().expect(...)`→`.expect_err(...)` patch remains exact, with expected-Err behavior and unexpected-Ok panic-format qualification preserved. Final source was not relinked or runtime-rerun. Native/CLI snapshots match exact Git source; preserved v0/v1/v2 manifests retain identical Native ELF2941f19a… and Native/CLI hashes. These are provenance checks, not a new production-source review.

All required qualifications remain explicit. Retained allocator ownership is separate from total request heap and allocator-internal realloc overlap. The source ring bound, real capacity-overflow reservation test, unchanged infallible header-clone behavior, startup accounting, final manifest→observer→cumulative→source-after→cleanup→resource-post order, and original receipts remain intact. The fault observer503 is supported by published `controls/faults-fit-v2-02/host.log` **line17**, `statistics failed: service accounting is incomplete`; accounting remains false and SQL/object-stat/estimate fields remainNULL. Eleven GC-order checks remain true; `unknown-sql` and `gateway-lost-reply` stay explicitly excluded/unclaimed/NULL.

Integer/string/bool/NULL receipt identity remains exact. The known-scope ledger enumerates exactly the same seven one-ULP paths and three underlying timing/advisory-cost values as the unchanged final independent audit. The original failed exact-comparison draft retains its four failures. Raw bytes and hashes are unchanged; the serialization explanation remains an inference and no broader tolerance or performance approval follows.

Historical first16 combined-resource FAIL/unknown threshold/missing original POST, overwritten successful-v0 PREs, blocked allocation-v1, fault-v1 missing final receipts, missing initial noncanonical library console, unmeasured per-push HTTP hashes/peak-object meter/whole-key ordered GET timeline, and original v2 lint failure remain preserved. The author's initial staged whitespace failure is retained in its original log; exact gzip carriers preserve the two original review diffs and the final diff check passes. Unknown Native compute, Worker CPU, DO duration and full bill remainNULL. This selected publication does not claim the complete volatile raw dataset or a new runtime result.

Only this new review directory and its fresh offline reconstruction were written. No primary/main access, source edit, build, runtime, service, control rerun, full raw rehash, CI, push, cloud/shadow/API task or delegation occurred. **STOP.**
'''
(OUT/'REVIEW.md').write_text(md)
outputs=[]
for p in sorted(OUT.iterdir()):
 if p.is_file() and p.name!='OUTPUT_SHA256SUMS':
  outputs.append(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p)+'\n')
(OUT/'OUTPUT_SHA256SUMS').write_text(''.join(outputs))
print(json.dumps({'verdict':verdict['verdict'],'blockers':verdict['concrete_blockers'],'review_MD_SHA256':hashlib.sha256((OUT/'REVIEW.md').read_bytes()).hexdigest(),'review_JSON_SHA256':hashlib.sha256((OUT/'REVIEW.json').read_bytes()).hexdigest(),'input_inventory_SHA256':hashlib.sha256((OUT/'INPUT_SHA256_INVENTORY.json').read_bytes()).hexdigest(),'all_checks_pass':True},indent=2))
