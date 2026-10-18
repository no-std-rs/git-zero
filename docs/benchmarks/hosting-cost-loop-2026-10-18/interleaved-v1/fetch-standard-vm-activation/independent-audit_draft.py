from pathlib import Path
import hashlib,json,tarfile,subprocess,os,yaml,re
OUT=Path('/tmp/git-zero-fetch-portability-independent-review')
HAND=Path('/tmp/git-zero-fetch-portability-handoff-20261018-01')
DRAFT=Path('/tmp/git-zero-fetch-standard-vm-draft-20261018-01')
CONTROL=Path('/tmp/git-zero-fetch-portability-export-control-20261018-01')
inputs={}
def digest(b):return hashlib.sha256(b).hexdigest()
def read(p):p=Path(p);b=p.read_bytes();inputs[str(p)]=digest(b);return b
def js(p):return json.loads(read(p))
wf=read(DRAFT/'fetch-phase-standard-vm.yml');assert wf==read(HAND/'draft/fetch-phase-standard-vm.yml')
assert read(DRAFT/'README.md')==read(HAND/'draft/README.md')
v=yaml.safe_load(wf);job=v['jobs']['diagnostic'];assert job['if']=='${{ false }}'
assert v['permissions']=={'contents':'read'} and v['concurrency']['cancel-in-progress'] is False
assert job['env']['DIAGNOSTIC_SOURCE']=='5d7f6a4c922ecca46129a391682e78d7b7aa5488'
assert job['env']['REFERENCE_GIT_VERSION']=='2.53.0' and job['env']['NODE_VERSION']=='22.20.0' and job['env']['RUST_TOOLCHAIN']=='1.97.1'
assert job['env']['CARGO_BUILD_JOBS']==2 and job['env']['CARGO_INCREMENTAL']==0
steps=job['steps'];runs=[s for s in steps if 'run' in s];assert len(runs)==4
for s in runs:
 assert s['shell']=='bash';p=subprocess.run(['/bin/bash','-n'],input=s['run'].encode(),stdout=subprocess.PIPE,stderr=subprocess.PIPE);assert p.returncode==0,(s['name'],p.stderr)
checks=[s for s in steps if s.get('uses','').startswith('actions/checkout@')];assert len(checks)==1 and checks[0]['with']['ref']==job['env']['DIAGNOSTIC_SOURCE'] and checks[0]['with']['persist-credentials'] is False
assert all(re.fullmatch(r'actions/[a-z-]+@[0-9a-f]{40}',s['uses']) for s in steps if 'uses' in s)
text=wf.decode()
for required in ['cargo +"$RUST_TOOLCHAIN" build --release --locked -p git-zero-host --bin repository-host --bin fetch-phase-supervisor -j2','test "$(git --version)" = "git version $REFERENCE_GIT_VERSION"','test "$(node --version)" = "v$NODE_VERSION"','npm ci --prefix examples/repository-host/cloudflare','5818bd7d80b061bbbdfec8a433d609dc8818a05991f731ffc4a561e2ca18c653','--seal-runtime-manifest','runtime_manifest_sha256:$runtime','replicas_each:1','source_memory_floor_bytes:8589934592','source_raw_reserve_bytes:2147483648','raw_cap_bytes:8589934592','proposed_launch_memory_floor_bytes:17179869184','proposed_launch_disk_floor_bytes:10737418240','findmnt -T "$RUNNER_TEMP" --json','test "$(findmnt -n -T "$RUNNER_TEMP" -o FSTYPE)" != tmpfs','test ! -e "$RAW"','ss -H -lnt','4128768','find "$base" -maxdepth 1 -type f','if: always()']:
 assert required in text,required
uploads=[s for s in steps if s.get('uses','').startswith('actions/upload-artifact@')];assert len(uploads)==3
for s in uploads:
 assert s['with']['path'].endswith('.tgz') and s['with']['compression-level']==0 and s['if']=='always()'
control=js(HAND/'export-control.json');assert control['pass_'] and control['draft_job_still_disabled'] and control['network_or_upload_performed'] is False
artifacts=[]
for row in control['artifacts']:
 p=Path(row['path']);b=read(p);assert len(b)==row['bytes']<=4128768 and digest(b)==row['sha256']
 spec=p.stem;base=CONTROL/('fetch-phase-preparation' if spec=='preparation' else 'fetch-phase-reviewed-raw/64mib-cold-0')
 members=[]
 with tarfile.open(p,'r:gz') as tf:
  for m in tf.getmembers():
   assert m.isfile() and '/' not in m.name and m.name not in ['.','..'] and not m.name.startswith('/')
   allowed=m.name.endswith(('.json','.jsonl','.headers','.body','.stderr','.txt','.sha256')) or m.name in ['native.stdout','wrangler.stdout','batch.stdout']
   assert allowed,m.name
   body=tf.extractfile(m).read();assert len(body)==m.size and not body.startswith(b'\x7fELF')
   assert body==read(base/m.name),(p,m.name)
   if m.name.endswith('.body'):assert isinstance(json.loads(body),dict)
   members.append(m.name)
 assert len(members)==row['members']
 listed=read(p.with_suffix('.files')).split(b'\0');listed=[s.decode() for s in listed if s]
 assert sorted(members)==sorted(listed)
 artifacts.append({'path':str(p),'bytes':len(b),'sha256':digest(b),'members':len(members),'all_members_flat_allowlist':True,'members_match_owned_copies':True,'no_ELF_or_raw_repository_service_tree':True})
result={'pass':True,'scope':'Disabled draft static review and existing compact-export-control archive inspection only','workflow_path':str(DRAFT/'fetch-phase-standard-vm.yml'),'workflow_sha256':digest(wf),'literal_false_fuse':True,'exact_source':'5d7f6a4c922ecca46129a391682e78d7b7aa5488','shell_steps_parsed_without_execution':4,'pins':{'git':'2.53.0','node':'22.20.0','rust':'1.97.1','wrangler':'4.149.0 npm-ci lock','profile':'release --locked -j2'},'git_tar_sha256':'5818bd7d80b061bbbdfec8a433d609dc8818a05991f731ffc4a561e2ca18c653','primary_source_checks':[{'url':'https://www.kernel.org/pub/software/scm/git/sha256sums.asc','observed_entry':'5818bd7d80b061bbbdfec8a433d609dc8818a05991f731ffc4a561e2ca18c653  git-2.53.0.tar.xz'},{'url':'https://nodejs.org/en/blog/release/v22.20.0','observed_release':'22.20.0'}],'proposed_unapproved_launch_memory_floor_bytes':17179869184,'proposed_unapproved_launch_disk_floor_bytes':10737418240,'source_per_boundary_memory_floor_bytes':8589934592,'source_raw_reserve_bytes':2147483648,'source_sampled_raw_cap_bytes':8589934592,'process_planning_headroom_bytes':4294967296,'proposed_launch_16GiB_not_current_authority':True,'future_root_discussion':{'memory_12GiB_equals_source_8GiB_plus_4GiB_headroom':True,'disk_10GiB_equals_raw_8GiB_plus_2GiB_reserve':True,'approval_given_by_this_review':False},'fixed_case_work_seconds':1200,'fixed_cleanup_seconds':5,'serial_cases':2,'replicas_each':1,'raw_mount_is_not_hard_RSS_or_disk_quota':True,'new_node22_release_VM_disk_cohort_prequalified':False,'compact_artifacts':artifacts,'input_bundle_cap_bytes':4128768,'future_final_artifact_service_cap_bytes':4194304,'future_final_artifact_service_bytes_observed':None,'ci_run_or_push_or_actual_upload':False,'reviewer_export_shell_executions':0}
(OUT/'draft-review.json').write_text(json.dumps(result,indent=2)+'\n')
(OUT/'draft-inputs.json').write_text(json.dumps(inputs,indent=2)+'\n')
print(json.dumps({'pass':True,'shell_steps_parsed':4,'fuse':job['if'],'artifacts':artifacts},indent=2))
