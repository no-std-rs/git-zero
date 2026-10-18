"""Offline exact-byte publication verification/reconstruction; starts no service/build.

Usage: python3 reconstruct-publication.py --publication PATH --output NEW_PATH
Original API ZIP wrappers, local combined capsule and evaluator ELF are excluded
from publication. Their identities remain in the published inventories.
"""
import argparse,gzip,hashlib,json,tarfile
from pathlib import Path,PurePosixPath
p=argparse.ArgumentParser();p.add_argument('--publication',type=Path,default=Path(__file__).resolve().parent);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
pub=a.publication.resolve();out=a.output.resolve();assert not out.exists(),out
def sha(b):return hashlib.sha256(b).hexdigest()
def safe(n):
 q=PurePosixPath(n);assert not q.is_absolute() and '..' not in q.parts and '\\' not in n and '\0' not in n,n
 return n.removeprefix('./').rstrip('/')
checks=0;seen=set()
for line in (pub/'SHA256SUMS').read_text().splitlines():
 h,n=line.split('  ',1);safe(n);assert n not in seen;seen.add(n);v=pub/n
 assert v.is_file() and not v.is_symlink() and sha(v.read_bytes())==h,n;checks+=1
assert {str(v.relative_to(pub)) for v in pub.rglob('*') if v.is_file()}==seen|{'SHA256SUMS'}
mapping=json.loads((pub/'ARTIFACT_MAP.json').read_text());out.mkdir(parents=True);restored=[]
for m in mapping['mapped_exact_payloads']:
 n=safe(m['source_path']);v=pub/safe(m['published_path']);b=v.read_bytes()
 assert len(b)==m['published_bytes'] and sha(b)==m['published_SHA256']
 if m['encoding']=='deterministic gzip of exact source bytes':b=gzip.decompress(b)
 else:assert m['encoding']=='original bytes'
 assert len(b)==m['source_bytes'] and sha(b)==m['source_exact_SHA256']
 dst=out/n;dst.parent.mkdir(parents=True,exist_ok=True);assert not dst.exists();dst.write_bytes(b);restored.append(n);checks+=1
archives=[out/m['source_path'] for m in mapping['mapped_exact_payloads'] if m['published_path'].startswith('original-archives/')];assert len(archives)==5
assert all(v.stat().st_size<=4194304 for v in archives) and sum(v.stat().st_size for v in archives)<=20971520
internal=0
for archive in archives:
 artifact=archive.parent.name
 if artifact=='hosting-scores-screen-1':dest=out/'scores'
 else:
  assert artifact.startswith('hosting-screen-') and artifact.endswith('-1')
  profile=artifact.removeprefix('hosting-screen-').removesuffix('-1');assert profile in ['16mib-cold','16mib-warm','64mib-cold','64mib-warm'];dest=out/'profiles'/profile
 dest.mkdir(parents=True,exist_ok=True);names=set();total=0
 with tarfile.open(archive) as tar:
  for m in tar.getmembers():
   n=safe(m.name);assert n not in names or not n;names.add(n)
   assert m.isfile() or m.isdir();assert not m.issym() and not m.islnk() and not m.sparse
   assert 0<=m.size<=128*1024*1024;total+=m.size;assert total<=512*1024*1024
  tar.extractall(dest,filter='data')
 sums=[]
 for line in (dest/'SHA256SUMS').read_text().splitlines():
  h,n=line.split('  ',1);n=safe(n);v=dest/n;assert n not in sums;sums.append(n)
  assert v.is_file() and not v.is_symlink() and sha(v.read_bytes())==h;internal+=1
 assert {str(v.relative_to(dest)) for v in dest.rglob('*') if v.is_file()}==set(sums)|{'SHA256SUMS'}
raw=list((out/'profiles').glob('*/*/*mib-*.json'));assert len(raw)==8
for line in (out/'RAW_SHA256SUMS').read_text().splitlines():
 h,n=line.split('  ',1);assert sha((out/safe(n)).read_bytes())==h;checks+=1
review=json.loads((out/'INDEPENDENT_REVIEW.json').read_text());assert review['all_scores_and_comparison_byte_exact']
assert review['full_hosting_cost_USD'] is None and not review['formal_acceptance_speed_plateau_or_stop_established']
result={'publication_files_verified':len(seen),'exact_mapped_payloads_restored':len(restored),'original_tar_payloads_restored':5,'raw_reports':8,'internal_original_checksums':internal,'checks':checks,'local_only_original_ZIP_wrappers':mapping['local_only_original_transport_wrappers'],'scope':'Exact offline reconstruction only. Frozen Rust evaluator identity and its previously byte-exact two scores+comparison are retained; no ELF is published, no service/build/benchmark starts. Final five original-R1/final pairs on each all4 profiles NOT DONE.'}
(out/'OFFLINE_RECONSTRUCTION.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({k:v for k,v in result.items() if k!='local_only_original_ZIP_wrappers'}))
