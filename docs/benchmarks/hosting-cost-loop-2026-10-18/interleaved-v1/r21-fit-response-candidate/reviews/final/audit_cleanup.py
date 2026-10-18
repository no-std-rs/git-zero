import pathlib,json,os,errno,datetime,hashlib
OUT=pathlib.Path('/tmp/git-zero-fit-response-independent-final-review')
RAW=pathlib.Path('/dev/shm/git-zero-fit-response-correctness-20261018')
claim=json.load(open('/tmp/git-zero-fit-response-owned-cleanup-final.json'));errors=[];pids=[]
for p in claim['recorded_current_scope_processes']:
    pid=p['recorded_pid'];present=pathlib.Path('/proc/'+str(pid)).exists()
    try:os.kill(-pid,0);group=True
    except ProcessLookupError:group=False
    except PermissionError:group=True
    pids.append({'recorded_pid':pid,'proc_present_now':present,'candidate_process_group_present_now':group})
    if present or group:errors.append({'reason':'recorded PID/candidate group present','pid':pid})
listeners=[]
for family in ['tcp','tcp6']:
    for line in pathlib.Path('/proc/net/'+family).read_text().splitlines()[1:]:
        cols=line.split();port=int(cols[1].rsplit(':',1)[1],16)
        if port in [8849,8850]and cols[3]=='0A':listeners.append({'family':family,'local':cols[1],'state':cols[3]})
if listeners:errors.append({'reason':'owned port listener','listeners':listeners})
old=RAW/'faults-fit-v1';missing={pattern:[str(x)for x in old.glob(pattern)]for pattern in ['copied-final.*','contract.json','source-after.json','cleanup.json']};missing['faults-fit-v1.resource-post.json']=[str(x)for x in RAW.glob('faults-fit-v1.resource-post.json')]
if any(missing.values()):errors.append({'reason':'original missing fault receipts appeared','matches':missing})
blocked=json.load(open(RAW/'allocation-fit-v1.resource-pre.json'));blocked_no_scope=not(RAW/'allocation-fit-v1').exists()
if not blocked_no_scope or blocked['MemAvailable']>=42949672960:errors.append({'reason':'blocked allocation evidence no longer matches'})
first16=json.load(open(RAW/'first16-evict-v0.resource.json'));pre_mtime=(RAW/'first16-evict-v0.resource.json').stat().st_mtime_ns;startup=(RAW/'first16-evict-v0/runtime-startup.json').stat().st_mtime_ns
if not pre_mtime<startup:errors.append({'reason':'first16 surviving resource receipt not PRE'})
summary={'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'kind':'separate independent actual host namespace read-only cleanup/gap observation; no process mutation or port bind','errors':errors,'recorded_processes_checked':pids,'owned_port_listeners_now':listeners,'no_global_process_absence_claim':True,'original_fault_missing_receipts_matches':missing,'blocked_allocation_original_PRE':blocked,'blocked_allocation_scope_absent':blocked_no_scope,'first16_eviction_surviving_receipt':first16,'first16_receipt_is_PRE':pre_mtime<startup,'first16_original_post_measurement':'absent; specific threshold remains UNKNOWN','raw_regular_allocated_budget_remaining':8589934592-7140257792,'raw_volatile_local_only':True}
(OUT/'cleanup-gap-audit.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({'errors':errors,'recorded_PIDs_checked':len(pids),'listeners':listeners,'old_fault_gaps_preserved':not any(missing.values()),'blocked_scope_absent':blocked_no_scope,'first16_surviving_PRE':pre_mtime<startup},indent=2))
