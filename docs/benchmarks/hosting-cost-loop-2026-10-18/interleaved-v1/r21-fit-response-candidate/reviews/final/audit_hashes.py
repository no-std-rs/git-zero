import pathlib,json,hashlib,os,stat,datetime
OUT=pathlib.Path('/tmp/git-zero-fit-response-independent-final-review')
HANDOFF=pathlib.Path('/tmp/git-zero-fit-response-completion-handoff.json')
j=json.load(open(HANDOFF));declared={};conflicts=[];errors=[];records={}
def add(path,v):
    if path in declared and declared[path].get('sha256')!=v.get('sha256'):
        conflicts.append({'path':path,'first':declared[path]['sha256'],'second':v['sha256']})
    declared[path]=v
def collect(v):
    if isinstance(v,dict):
        if isinstance(v.get('path'),str)and v['path'].startswith('/')and'sha256'in v:add(v['path'],v)
        for x in v.values():collect(x)
    elif isinstance(v,list):
        for x in v:collect(x)
collect(j)
add(str(HANDOFF),{'sha256':'3ce29e5f8a639840ea12e4a2ccb5d082fd239334da5db69e9ab231686fccc349'})
add(str(HANDOFF.with_suffix('.md')),{'sha256':'f928978b2bd28c484a95e85786b6da40f6f7fd425bdd11f06a1af7156e955fec'})
inventories=[]
for typ in ['raw','compact']:
    inv=json.load(open(j['inventories'][typ]['path']));inventories.append(inv);collect(inv)
for version in ['v0','v1','v2']:
    root=pathlib.Path('/tmp/git-zero-fit-response-build-'+version);m=json.load(open(root/'manifest.json'))
    for name,v in m['files'].items():add(str(root/name),v)
for root in ['/tmp/git-zero-fit-response-independent-review','/tmp/git-zero-fit-response-independent-review-supplement-01']:
    for name in ['REVIEW.md','REVIEW.json','INPUT_SHA256_INVENTORY.json','OUTPUT_SHA256SUMS']:
        p=pathlib.Path(root)/name
        add(str(p),{'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
def record(path):
    path=str(path)
    if path in records:return records[path]
    before=os.stat(path);h=hashlib.sha256()
    with open(path,'rb')as f:
        while b:=f.read(1024*1024):h.update(b)
    s=os.stat(path)
    if(before.st_size,before.st_mtime_ns)!=(s.st_size,s.st_mtime_ns):errors.append({'path':path,'error':'changed_during_read'})
    r={'path':path,'bytes':s.st_size,'allocated_bytes':s.st_blocks*512,'mtime_ns':s.st_mtime_ns,'sha256':h.hexdigest()};records[path]=r;return r
for path,d in sorted(declared.items()):
    try:r=record(path)
    except FileNotFoundError:errors.append({'path':path,'error':'missing'});continue
    for k in ['sha256','bytes','allocated_bytes','mtime_ns']:
        if k in d and r[k]!=d[k]:errors.append({'path':path,'field':k,'expected':d[k],'actual':r[k]})
    if'mtime_epoch'in d:
        sec,frac=d['mtime_epoch'].split('.');ns=int(sec)*1000000000+int(frac[:9].ljust(9,'0'))
        if r['mtime_ns']!=ns:errors.append({'path':path,'field':'mtime_epoch','expected_ns':ns,'actual_ns':r['mtime_ns']})
sets=[]
for inv in inventories:
    root=pathlib.Path(inv['root']);actual=set()
    for base,dirs,files in os.walk(root,followlinks=False):
        for name in files:
            p=pathlib.Path(base)/name
            if stat.S_ISREG(p.lstat().st_mode):actual.add(str(p))
    expected={r['path']for r in inv['files']};rs=[records[p]for p in expected if p in records]
    sets.append({'root':str(root),'actual_regular_files':len(actual),'declared_files':len(expected),'extra':sorted(actual-expected),'missing':sorted(expected-actual),'actual_logical_bytes':sum(r['bytes']for r in rs),'actual_allocated_bytes':sum(r['allocated_bytes']for r in rs)})
# Compare all files of both older seals against final raw/mirror hashes. New files
# are allowed; no earlier evidence may silently change.
historical=[]
for name in ['raw-evidence-inventory.json','raw-evidence-inventory-supplement-01.json','compact-mirror-inventory.json']:
    p=pathlib.Path('/tmp/git-zero-fit-response-'+name);record(p);old=json.load(open(p));mismatch=[]
    for d in old['files']:
        r=record(d['path'])
        if r['sha256']!=d['sha256']or r['bytes']!=d['bytes']:mismatch.append(d['path'])
    historical.append({'inventory':str(p),'old_file_count':len(old['files']),'changed_or_missing':mismatch})
summary={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'mode':'independent actual host namespace read-only streamed SHA audit; no runtime/build','input_records':len(records),'errors':errors,'declaration_conflicts':conflicts,'inventory_sets':sets,'historical_seal_continuity':historical}
(OUT/'hash-audit.json').write_text(json.dumps(summary,indent=2)+'\n')
(OUT/'INPUT_SHA256_INVENTORY.json').write_text(json.dumps({'audit':summary,'files':sorted(records.values(),key=lambda r:r['path'])},indent=2)+'\n')
(OUT/'INPUT_SHA256SUMS').write_text(''.join(r['sha256']+'  '+r['path']+'\n'for r in sorted(records.values(),key=lambda r:r['path'])))
print(json.dumps(summary,indent=2))
