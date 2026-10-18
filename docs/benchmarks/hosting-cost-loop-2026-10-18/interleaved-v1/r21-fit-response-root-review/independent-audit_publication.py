#!/usr/bin/env python3
"""Independent read-only audit; writes only this fresh review directory."""
import collections
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import tarfile

OUT = Path(__file__).parent
REPO = Path('/tmp/git-zero-hosting-fit-response')
REL = 'docs/benchmarks/hosting-cost-loop-2026-10-18/interleaved-v1/r21-fit-response-candidate'
PUB = REPO / REL
COMMIT = 'd573f463655083885a3cd51a76761c7967834777'
PARENT = '95fdf58f6bd7298736cfe124725e4f7e6c9aceb9'
inputs = {}
checks = {}
errors = []
decoded = {}

def sha(b):
    return hashlib.sha256(b).hexdigest()

def read(p):
    p = Path(p)
    b = p.read_bytes()
    inputs[str(p)] = {'bytes': len(b), 'sha256': sha(b)}
    return b

def check(name, condition, detail=None):
    checks[name] = {'pass': bool(condition), 'detail': detail}
    if not condition:
        errors.append(name)

def git(*args):
    env = dict(os.environ, GIT_OPTIONAL_LOCKS='0', GIT_PAGER='cat')
    return subprocess.check_output(['git', '-C', str(REPO), *args], env=env)

def safe(name):
    p = PurePosixPath(name)
    return bool(name) and not p.is_absolute() and '..' not in p.parts and not any(c in name for c in '\n\r\t\\')

signed = {
 '/tmp/git-zero-fit-response-ledger-handoff.md': '2fdce49fb5d0007d7daa90cd4eb4bc9487aa34328a41b63c27cfeb26459f7d7c',
 '/tmp/git-zero-fit-response-ledger-handoff.json': '26619847e64bccc979804b4651c3fb63af946a78fcc2c679413bd843e3421368',
 '/tmp/git-zero-fit-response-r21-ledger-file-map.json': '039c26ad1a399d19da1ded27a70d4b0e14cbf5203aca4af91ad3b837d1d57c84',
 '/tmp/git-zero-fit-response-independent-final-review/REVIEW.md': '8993997429a866f529e46aac775b39f9967e6b4b396c58e256483b29bd46e7c1',
 '/tmp/git-zero-fit-response-independent-final-review/REVIEW.json': 'fe56cbfb0014bd3044626a6be0b556b0c0e3a7006a0d654137ecc6c4ffb782cc',
 '/tmp/git-zero-fit-response-independent-final-review/INPUT_SHA256_INVENTORY.json': '8102c427d8637aa5a4987c9df01712dc5d5ea47189a583c5d3fb43892f51daba',
}
for p,h in signed.items():
    check('signed:'+p, sha(read(p)) == h)
handoff = json.loads(read('/tmp/git-zero-fit-response-ledger-handoff.json'))
filemap = json.loads(read('/tmp/git-zero-fit-response-r21-ledger-file-map.json'))
check('embedded_filemap_exact', handoff['exact_committed_file_map'] == filemap)
check('filemap_totals', filemap['regular_files'] == 116 and filemap['stored_bytes'] == 6976776 and len(filemap['files']) == 116)
actual_paths = {str(p.relative_to(REPO)) for p in PUB.rglob('*') if p.is_file()}
map_paths = {x['repository_path'] for x in filemap['files']}
check('exact_working_tree_files', actual_paths == map_paths and not any(p.is_symlink() for p in PUB.rglob('*')))
publication = []
for x in filemap['files']:
    b = read(REPO/x['repository_path'])
    blob = hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()
    check('stored:'+x['repository_path'], len(b) == x['bytes'] and sha(b) == x['sha256'] and blob == x['Git_blob'])
    publication.append({'path': x['repository_path'], 'bytes': len(b), 'sha256': sha(b), 'Git_blob': blob})
check('actual_stored_bytes', sum(x['bytes'] for x in publication) == 6976776)
tree = {}
for line in git('ls-tree','-r','-z',COMMIT,'--',REL).split(b'\0'):
    if not line: continue
    meta,p = line.split(b'\t'); mode,kind,blob = meta.decode().split()
    tree[p.decode()] = {'mode':mode, 'kind':kind, 'blob':blob}
check('exact_commit_tree', set(tree)==map_paths and all(tree[x['path']]['blob']==x['Git_blob'] and tree[x['path']]['kind']=='blob' and tree[x['path']]['mode'] in ('100644','100755') for x in publication))
check('head_parent_clean', git('rev-parse','HEAD').decode().strip()==COMMIT and git('rev-parse',COMMIT+'^').decode().strip()==PARENT and git('status','--porcelain=v1')==b'')
diff = git('diff-tree','--no-commit-id','--name-status','-r',COMMIT).decode().splitlines()
check('docs_only_additions', len(diff)==116 and all(line.startswith('A\t'+REL+'/') for line in diff))
ancestry = handoff['ancestry']
check('exact_source_ancestry', all(git('rev-parse',ancestry[i]+'^').decode().strip()==ancestry[i-1] for i in range(1,len(ancestry))))
native_tree='4717edd249bcb85edec47674787aa1ecf9f7d8f1'
worker_tree='7f41db4407963ed04e6cbe19556b1416633caf18'
tree_matches = {}
for line in git('ls-tree','-r','-t',COMMIT).decode().splitlines():
    meta,p=line.split('\t'); mode,kind,h=meta.split()
    if h in (native_tree,worker_tree): tree_matches[h]=p
check('production_tree_objects_present', set(tree_matches)=={native_tree,worker_tree},tree_matches)
check('production_tree_objects_unchanged', all(git('rev-parse',PARENT+':'+p).decode().strip()==h for h,p in tree_matches.items()))

manifest=json.loads(read(PUB/'PUBLICATION-MANIFEST.json'))
check('publication_manifest', {x['path'] for x in manifest['files']}=={str(Path(p).relative_to(REL)) for p in map_paths}-{ 'PUBLICATION-MANIFEST.json','SHA256SUMS'} and len(manifest['files'])==114 and sum(x['bytes'] for x in manifest['files'])==manifest['stored_bytes']==6943752 and manifest['source_approved_parent']==PARENT)
for x in manifest['files']:
    b=read(PUB/x['path']);check('manifest:'+x['path'],len(b)==x['bytes'] and sha(b)==x['sha256'])
sums=read(PUB/'SHA256SUMS').decode().splitlines()
sum_names=[]
for line in sums:
    h,p=line.split('  ',1);check('root_checksum:'+p,safe(p) and sha(read(PUB/p))==h);sum_names.append(p)
check('outer_checksum_coverage', len(sum_names)==115 and {str(PurePosixPath(p)) for p in sum_names}=={str(Path(p).relative_to(REL)) for p in map_paths}-{'SHA256SUMS'})

copies=json.loads(read(PUB/'COPY-MAP.json'))['files']
archive=read(PUB/'selected-evidence.tar.gz')
check('canonical_archive_identity', len(archive)==753056 and sha(archive)=='9983f879ac58a988b0a2a3bc6459987f995ed0264d766f49a3b36af4381f16cd')
with tarfile.open(fileobj=io.BytesIO(archive),mode='r:gz') as tf:
    members=tf.getmembers()
    regular=[m for m in members if m.isfile()]
    dirs=[m for m in members if m.isdir()]
    check('safe_archive_members',len(members)==960 and len(regular)==947 and len(dirs)==13 and len({m.name for m in members})==960 and all(safe(m.name) and (m.isfile() or m.isdir()) for m in members))
    payload={m.name:tf.extractfile(m).read() for m in regular}
archive_names={x['archive_member'] for x in copies if x['encoding']=='tar-member'}
check('exact_archive_file_set',len(archive_names)==946 and set(payload)==archive_names|{'SHA256SUMS'})
expected_internal=''.join(x['source_sha256']+'  '+x['archive_member']+'\n' for x in copies if x['encoding']=='tar-member').encode()
check('exact_internal_checksum_manifest',payload['SHA256SUMS']==expected_internal)
for line in payload['SHA256SUMS'].decode().splitlines():
    h,p=line.split('  ',1);check('internal_checksum:'+p,sha(payload[p])==h)

final_seal=json.loads(read('/tmp/git-zero-fit-response-independent-final-review/INPUT_SHA256_INVENTORY.json'))
sealed={x['path']:x for x in final_seal['files']}
check('sealed_final_review_no_errors',not final_seal['audit']['errors'] and not final_seal['audit']['declaration_conflicts'])
bindings=[]; binaries=[]; packs=[]; totals=collections.Counter(); direct_count=0; raw_sealed_count=0
for x in copies:
    check('safe_carrier:'+x['source_path'],safe(x['stored_path']))
    carrier=read(PUB/x['stored_path'])
    encoding=x['encoding']
    if encoding=='tar-member':
        data=payload[x['archive_member']]
        check('member_binding:'+x['source_path'],x['member_bytes']==len(data) and x['member_sha256']==sha(data))
    else:
        check('carrier:'+x['stored_path'],x['stored_bytes']==len(carrier) and x['stored_sha256']==sha(carrier))
        data=gzip.decompress(carrier) if encoding=='gzip' else carrier
        if encoding=='gzip': check('deterministic_gzip_header:'+x['stored_path'],carrier[3]==0 and carrier[4:8]==b'\0'*4)
    check('decoded:'+x['source_path'],len(data)==x['source_bytes'] and sha(data)==x['source_sha256'])
    decoded[(x['stored_path'],x.get('archive_member',''))]=data
    direct=Path(x['source_path']).is_file()
    in_seal=x['source_path'] in sealed
    if direct:
        original=read(x['source_path']);check('actual_original:'+x['source_path'],original==data);direct_count+=1
    if in_seal:
        s=sealed[x['source_path']];check('signed_original_binding:'+x['source_path'],s['bytes']==len(data) and s['sha256']==sha(data))
    if not direct:
        check('sealed_raw_source_binding:'+x['source_path'],in_seal);raw_sealed_count+=1
    try: data.decode('utf-8');utf8=True
    except UnicodeDecodeError: utf8=False
    sig=b'PACK\0\0\0\2';offsets=[];start=0
    while True:
        pos=data.find(sig,start)
        if pos<0: break
        offsets.append(pos);start=pos+1
    check('classification:'+x['source_path'],x['UTF8']==utf8 and x['PACK_v2_signature']==bool(offsets) and x['PACK_v2_offsets']==offsets)
    check('payload_size_and_type:'+x['source_path'],len(data)<=(65536 if x['kind']=='wire' else 8388608) and not data.startswith((b'\x7fELF',b'SQLite format 3\0')) and not any(p in PurePosixPath(x['source_path']).parts for p in ('.git','.wrangler','node_modules')) and not x['source_path'].endswith(('.pack','.sqlite','.db','.tar','.zip')))
    if not utf8: binaries.append(x)
    if offsets:
        check('positive_PACK_classification:'+x['source_path'],x['kind']=='wire' and x['source_path'].endswith(('.request','.body')));packs.append(x)
    totals[encoding]+=1;totals['source_bytes']+=len(data)
    bindings.append({'source_path':x['source_path'],'stored_path':x['stored_path'],'encoding':encoding,'archive_member':x.get('archive_member'),'bytes':len(data),'sha256':sha(data),'source_binding':'actual original byte comparison'+(' and signed final input seal' if in_seal else '') if direct else 'signed final SOURCE-review input seal; host raw not rehashed'})
check('copy_totals',len(copies)==1051 and totals['source_bytes']==29775842 and totals['tar-member']==946 and totals['gzip']==78 and totals['identity']==27 and len(decoded)==1051)
check('all_copy_original_bindings',direct_count==105 and raw_sealed_count==946)
protocol=json.loads(read(PUB/'protocol-evidence-inventory.json'))
check('exact_positive_protocol_inventory',protocol['PACK_v2_bearing_files']==packs and len(packs)==len(binaries)==82 and {x['source_path'] for x in packs}=={x['source_path'] for x in binaries} and sum(x['source_bytes'] for x in packs)==41706 and min(x['source_bytes'] for x in packs)==323 and max(x['source_bytes'] for x in packs)==2766 and sum(x['source_path'].endswith('.body') for x in packs)==15 and sum(x['source_path'].endswith('.request') for x in packs)==67)
check('no_individual_control_carriers',not any(str(Path(p).relative_to(REL)).startswith('controls/') for p in map_paths) and len({x['stored_path'] for x in copies if x['encoding']=='tar-member'})==1)

source_map={'native.rs':'examples/repository-host/src/native.rs','cli.rs':'examples/repository-host/src/bin/repository-host.rs','runner.rs':'examples/repository-host/tests/multi_chunk_cloudflare.rs'}
for n,p in source_map.items():
    stored='source-maps/'+('final-runner.rs.gz' if n=='runner.rs' else n+'.gz')
    b=gzip.decompress(read(PUB/stored));check('final_snapshot_Git:'+n,b==git('show',PARENT+':'+p)==git('show',COMMIT+':'+p))
for v,c in [('v0','5b523c9c9b8d8b8876281371c888d5af77757df2'),('v1','5b523c9c9b8d8b8876281371c888d5af77757df2'),('v2','8287ef863e8ec78950ed3a9a59d509a7ed6e1dc2')]:
    m=json.loads(gzip.decompress(read(PUB/f'source-maps/{v}-manifest.json.gz')))
    b=gzip.decompress(read(PUB/f'source-maps/{v}-runner.rs.gz'))
    check('runner_manifest:'+v,sha(b)==m['files']['runner.rs']['sha256'] and len(b)==m['files']['runner.rs']['bytes'])
    if v=='v2':check('tested_v2_runner_Git',b==git('show',c+':'+source_map['runner.rs']))

for k in ('initial_failure_log','final_diff_log','sealed_portable_verification_log'):
    x=handoff['checks'][k];check('historical_publication_log:'+k,sha(read(x['path']))==x['sha256'])
read('/tmp/git-zero-r21-root-preintegration-check.json')
report={'commit':COMMIT,'parent':PARENT,'publication_regular_files':116,'publication_stored_bytes':6976776,'source_originals':len(copies),'source_original_bytes':totals['source_bytes'],'actual_original_byte_comparisons':direct_count,'signed_raw_original_bindings_without_rehash':raw_sealed_count,'archive_members':len(members),'archive_regular_files':len(regular),'archive_directory_headers':len(dirs),'archive_control_originals':len(archive_names),'PACK_files':len(packs),'PACK_bytes':sum(x['source_bytes'] for x in packs),'production_tree_paths':tree_matches,'checks':checks,'errors':errors}
(OUT/'publication-audit.json').write_text(json.dumps(report,indent=2)+'\n')
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps({'read_inputs':[dict(path=p,**x) for p,x in sorted(inputs.items())],'decoded_original_bindings':bindings,'raw_rehash_performed':False,'raw_original_binding_authority':'signed exact final SOURCE-review INPUT_SHA256_INVENTORY.json SHA8102c427d8637aa5a4987c9df01712dc5d5ea47189a583c5d3fb43892f51daba'},indent=2)+'\n')
(OUT/'INPUT_SHA256SUMS').write_text(''.join(x['sha256']+'  '+p+'\n' for p,x in sorted(inputs.items())))
print(json.dumps({k:v for k,v in report.items() if k!='checks'},indent=2))
print('Check count:',len(checks),'Input file count:',len(inputs))
if errors: raise SystemExit(1)
