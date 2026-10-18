import hashlib
import json
import os
import pathlib
import stat
import datetime

OUT = pathlib.Path('/tmp/git-zero-fit-response-independent-review')
HANDOFF = pathlib.Path('/tmp/git-zero-fit-response-source-handoff.json')
handoff = json.loads(HANDOFF.read_text())
inventory_paths = [pathlib.Path('/tmp/git-zero-fit-response-raw-evidence-inventory.json'), pathlib.Path('/tmp/git-zero-fit-response-compact-mirror-inventory.json')]
declared = {}
def collect(v):
    if isinstance(v, dict):
        if isinstance(v.get('path'), str) and v['path'].startswith('/') and 'sha256' in v:
            declared[v['path']] = v
        for x in v.values(): collect(x)
    elif isinstance(v, list):
        for x in v: collect(x)
collect(handoff)
declared[str(HANDOFF)] = {'sha256': 'c4fb7d4499088f6fe4d92f35f40589251fb85339e1083598641fc2872ab28ab1'}
declared['/tmp/git-zero-fit-response-source-handoff.md'] = {'sha256': 'cdb1215890e3d6acd2557cca3fe52bc13fe7bd5d63ab4261100e8afca3c7edff'}
inventories = []
for p in inventory_paths:
    j=json.loads(p.read_text()); inventories.append(j); collect(j)
records=[]; mismatches=[]; missing=[]
for p,d in sorted(declared.items()):
    try:
        before=os.stat(p)
        if not stat.S_ISREG(before.st_mode):
            mismatches.append({'path':p,'reason':'not_regular'}); continue
        h=hashlib.sha256()
        with open(p,'rb') as f:
            while data := f.read(1024*1024): h.update(data)
        after=os.stat(p)
        r={'path':p,'bytes':after.st_size,'mtime_ns':after.st_mtime_ns,'allocated_bytes':after.st_blocks*512,'sha256':h.hexdigest()}
        records.append(r)
        fields={k:{'declared':d[k],'actual':r[k]} for k in ('bytes','sha256','mtime_ns','allocated_bytes') if k in d and d[k]!=r[k]}
        if (before.st_size,before.st_mtime_ns)!=(after.st_size,after.st_mtime_ns): fields['stability']='changed_during_read'
        if fields: mismatches.append({'path':p,'fields':fields})
    except FileNotFoundError: missing.append(p)
raw=inventories[0]; compact=inventories[1]
sets=[]
for inv in inventories:
    root=pathlib.Path(inv['root'])
    actual={str(p) for p in root.rglob('*') if p.is_file()}
    expected={v['path'] for v in inv['files']}
    sets.append({'root':str(root),'actual_files':len(actual),'declared_files':len(expected),'extra':sorted(actual-expected),'missing':sorted(expected-actual)})
summary={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'mode':'read-only sealed evidence hash audit; no runtime/build','recorded_inputs':len(records),'mismatches':mismatches,'missing':missing,'inventory_sets':sets,'raw_actual_files':sets[0]['actual_files'],'raw_actual_logical_bytes':sum(r['bytes'] for r in records if r['path'].startswith(raw['root']+'/')),'raw_actual_allocated_bytes':sum(r['allocated_bytes'] for r in records if r['path'].startswith(raw['root']+'/'))}
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps({'audit':summary,'files':records},indent=2)+'\n')
(OUT/'input-audit-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps(summary,indent=2))
