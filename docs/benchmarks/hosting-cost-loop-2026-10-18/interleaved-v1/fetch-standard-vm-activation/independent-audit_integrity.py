from pathlib import Path
import hashlib, json, os, stat, subprocess, datetime, re
OUT=Path('/tmp/git-zero-fetch-portability-independent-review')
SRC=Path('/tmp/git-zero-hosting-fetch-diagnostics')
HAND=Path('/tmp/git-zero-fetch-portability-handoff-20261018-01')
SOURCE='5d7f6a4c922ecca46129a391682e78d7b7aa5488'
PARENT='e41159b7fcd3fc9ef16b189464fd6c54189af13b'
APPROVED='a827df032e87374737b1ac7e36fb518b507e8411'
inputs={}
def digest(b):return hashlib.sha256(b).hexdigest()
def checkfile(p, expected=None):
 p=Path(p); h=hashlib.sha256()
 with p.open('rb') as f:
  for b in iter(lambda:f.read(1048576),b''):h.update(b)
 got=h.hexdigest();inputs[str(p)]=got
 if expected is not None:assert got==expected,(str(p),got,expected)
 return got
def read(p):p=Path(p);b=p.read_bytes();inputs[str(p)]=digest(b);return b
def js(p):return json.loads(read(p))
def git(*args):return subprocess.check_output(['/usr/bin/git','-C',str(SRC),*args],env=dict(os.environ,GIT_OPTIONAL_LOCKS='0'))
def tree(commit):
 rows=[]
 for row in git('ls-tree','-rz','--full-tree',commit).split(b'\0'):
  if not row:continue
  meta,rel=row.split(b'\t',1);mode,kind,oid=meta.split()
  assert kind==b'blob',(commit,row)
  rows.append((mode.decode(),rel.decode(),oid.decode()))
 return rows
def blobs(specs):
 raw=subprocess.check_output(['/usr/bin/git','-C',str(SRC),'cat-file','--batch'],input=('\n'.join(specs)+'\n').encode(),env=dict(os.environ,GIT_OPTIONAL_LOCKS='0'))
 out=[];at=0
 for s in specs:
  end=raw.index(b'\n',at);head=raw[at:end].split();assert len(head)==3,(s,head)
  size=int(head[2]);out.append(raw[end+1:end+1+size]);at=end+size+2
 assert at==len(raw)
 return out
assert git('rev-parse','HEAD','HEAD^').decode().splitlines()==[SOURCE,PARENT]
assert git('status','--porcelain=v1')==b''
hand=js(HAND/'final-integrity.json')
checkfile(HAND/'HANDOFF.md','7071799fcea7af67250e6ff7c724cd52843dee12dd7ca75066639f21b1522eb6')
checkfile(HAND/'inputs.sha256','3178a273e9612c94318aa44bae75079f1526376b8273bfffd3020a25290d5d7a')
mirror=[]
for line in read(HAND/'inputs.sha256').decode().splitlines():
 h,p=line.split('  ',1);checkfile(p,h);mirror.append(Path(p))
assert len(mirror)==32
assert sum(p.stat().st_size for p in mirror)+(HAND/'inputs.sha256').stat().st_size==312600
for p,h in hand['prior_authorities_rehashed'].items():checkfile(p,h)
checkfile('/tmp/git-zero-fetch-stage3-correction-independent-review-20261018/REVIEW.md','40212fa993f379001f620e02715ca976e3dd93684ad0630735bf29644538c1b7')
for p in ['/tmp/git-zero-fetch-stage3-independent-review/REVIEW.md','/tmp/git-zero-fetch-stage3-independent-review/REVIEW.json','/tmp/git-zero-fetch-stage3-correction-independent-review-20261018/REVIEW.json','/tmp/git-zero-fetch-reviewed-batch02-independent-review/REVIEW.json','/tmp/git-zero-fetch-stage2-independent-review/REVIEW.md']:
 checkfile(p)
archives=[]
for name,expected,count in [('portability-tested-v1','9c44f153c9842041a44f96da7d91c681724041e24b2c5ddc300e3d7be7a92821',695),('portability-'+SOURCE+'-v1','86ff4746273a6d53acb95d0bb13d0f5c311bdfa9360ffcf10c45b063f5e3a1b5',697)]:
 d=Path('/tmp/git-zero-fetch-diagnostics-immutable')/name
 checkfile(d/'relocation-map.json',expected);m=js(d/'relocation-map.json');assert len(m['entries'])==count
 for e in m['entries']:
  p=Path(e['immutable_path']);checkfile(p,e['sha256']);s=p.stat();assert stat.S_ISREG(s.st_mode) and not(s.st_mode&0o222),(p,s.st_mode)
  original=Path(e['historical_original_path'])
  if original.exists():assert (s.st_dev,s.st_ino)!=(original.stat().st_dev,original.stat().st_ino),str(p)
 archives.append({'path':str(d),'map_sha256':expected,'aliases_verified':count,'all_readonly_independent_inodes':True})
final=js(Path(archives[-1]['path'])/'relocation-map.json')
rows=tree(SOURCE);data=blobs([oid for mode,rel,oid in rows]);assert len(rows)==694
for (mode,rel,oid),b in zip(rows,data):
 p=SRC/rel
 got=os.readlink(p).encode() if mode=='120000' else read(p)
 assert got==b,rel
 assert read(Path(archives[-1]['path'])/'source'/rel)==b,('archive',rel)
oldrows=tree(PARENT);olddata=blobs([oid for mode,rel,oid in oldrows]);old={rel:b for (_,rel,_),b in zip(oldrows,olddata)}
changed=[rel for (_,rel,_),b in zip(rows,data) if old.get(rel)!=b]
assert len(changed)==8,changed
approved_path=SRC/'examples/repository-host/src/bin/fetch-phase-supervisor/approved-source.sha256'
closure=read(approved_path);assert digest(closure)=='4e8283e57be8c2b4e01c35b427de54c23c73ec62e344340aa2b970ce35330632'
entries=[line.split('  ',1) for line in closure.decode().splitlines()];assert len(entries)==71 and len({p for h,p in entries})==71
approved_data=blobs([APPROVED+':'+p for h,p in entries])
for (h,p),b in zip(entries,approved_data):
 checkfile(SRC/p,h);assert digest(b)==h,('approved source',p)
closure_paths={p for h,p in entries}
all_core={p for _,p,_ in rows if p.startswith('crates/git-zero-core/src/') and p.endswith('.rs')}
assert all_core<=closure_paths
all_native={p for _,p,_ in rows if p.startswith('examples/repository-host/src/native/') and p.endswith('.rs')}
assert all_native<=closure_paths
for p in ['examples/repository-host/src/lib.rs','examples/repository-host/src/native.rs','examples/repository-host/src/diagnostics.rs','examples/repository-host/src/accounting.rs','examples/repository-host/src/socket_rpc.rs','examples/repository-host/src/bin/repository-host.rs','examples/repository-host/Cargo.toml','Cargo.lock','Cargo.toml']:
 assert p in closure_paths,p
manifest=js(HAND/'runtime-manifest.json');assert len(read(HAND/'runtime-manifest.json'))<=128*1024
actual={}
for role,e in manifest['binaries'].items():
 p=Path(e['path']);assert p.resolve()==p;actual[role]={'path':str(p),'sha256':checkfile(p,e['sha256']),'bytes':p.stat().st_size}
for e in manifest['runner_source_files']:
 checkfile(SRC/'examples/repository-host'/e['relative'],e['sha256'])
stage1=Path('/tmp/git-zero-fetch-diagnostics-target-j2/debug/fetch-phase-diagnostic')
checkfile(stage1,'d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943')
for p,h in [('64mib-cold-history_no_haves_16.json','852558c0bcd8491061b762ac2a50352a89565d927fe2ae367e9e9dc22ad713a4'),('16mib-warm-history_no_haves_32.json','5f720f8368e75f4d9447871ce6809330e4f8db188d561c3457de51e21bf3072d')]:
 checkfile(SRC/'examples/repository-host/src/bin/fetch-phase-supervisor/plans'/p,h)
# Compare top-level items byte-for-byte using their declaration boundaries.
def items(b):
 text=b.decode();starts=list(re.finditer(r'(?m)^(?:pub\(super\) )?(?:fn|struct|impl|mod) [A-Za-z_][A-Za-z_0-9]*',text));out={}
 for i,m in enumerate(starts):
  name=m.group().split()[-1];end=starts[i+1].start() if i+1<len(starts) else len(text)
  out[name]=text[m.start():end].strip()
 return out
rel='examples/repository-host/src/bin/fetch-phase-supervisor/replay.rs'; before=items(old[rel]);after=items(read(SRC/rel))
unchanged=['footprint','capacity','capacity_with_helper','pipe_capture','command','git','text','content','geometry','fixture','tiny_plan','http','close_scopes','operation','worker','resource_check','blocked_resource_helper','blocked_resource_proof','tiny']
for name in unchanged:assert before[name]==after[name],name
rel='examples/repository-host/src/bin/fetch-phase-supervisor.rs';before=items(old[rel]);after=items(read(SRC/rel));super_unchanged=[k for k in before if k not in ['inherited_environment_receipt','main','replay']]
for name in super_unchanged:assert before[name]==after[name],name
# Main's only behavior addition is explicit sealing entrypoint.
assert before['main'].replace('        "--tiny-production-proof"', '        "--seal-runtime-manifest" if args.len() == 2 => runtime::seal(&root),\n        "--tiny-production-proof"')==after['main']
checks={'source':SOURCE,'parent':PARENT,'source_clean':True,'tracked_blobs_and_final_source_aliases_verified':len(rows),'parent_tracked_blobs':len(oldrows),'unchanged_parent_files':len(rows)-len(changed),'changed_paths':changed,'mirror_entries':len(mirror),'mirror_bytes':sum(p.stat().st_size for p in mirror),'archives':archives,'approved_source_closure_entries':len(entries),'approved_source_commit':APPROVED,'approved_source_closure_sha256':digest(closure),'complete_core_source_files':len(all_core),'native_submodule_files':len(all_native),'runtime_manifest_sha256':checkfile(HAND/'runtime-manifest.json'),'actual_binaries':actual,'unchanged_executor_functions':unchanged,'unchanged_supervisor_items':super_unchanged,'only_main_seal_entrypoint_added':True,'build_source_commit_at_seal':manifest['build_source_commit_at_seal'],'build_profile':manifest['build_profile'],'reviewer_builds_or_workload_runs':0,'observed_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()}
(OUT/'integrity.json').write_text(json.dumps(checks,indent=2)+'\n')
(OUT/'integrity-inputs.json').write_text(json.dumps(inputs,indent=2)+'\n')
print(json.dumps({k:checks[k] for k in ['source','source_clean','tracked_blobs_and_final_source_aliases_verified','changed_paths','mirror_entries','mirror_bytes','approved_source_closure_entries','complete_core_source_files','native_submodule_files','runtime_manifest_sha256']},indent=2))
