# SPDX-License-Identifier: MIT
"""Bounded once-owned test process group retirement.
No killpg(0), post-reap signals, historical PID retries or command arguments.
"""
import hashlib,os,re,resource,signal,subprocess,time

sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()

def inventory_file_limit():
    resource.setrlimit(resource.RLIMIT_FSIZE,(2*1024*1024,2*1024*1024))

def stop_owned_group(owned,out,label):
    assert owned['cleanupAttempted'] is True
    if 'groupEvidence' in owned:return owned['groupEvidence']['stopped']
    assert re.fullmatch(r'[A-Za-z0-9._-]{1,128}',label)
    child=owned['child']
    state={'pgid':child.pid,'uid':os.getuid(),'leaderReturnCode':child.poll(),
        'stopped':False,'absenceSnapshot':None,'inventories':[],'signals':[],'unknownReason':None}
    owned['groupEvidence']=state

    def inventory(stage):
        index=len(state['inventories'])+1
        assert index<=5,'Bounded current inventory count exceeded'
        stdout=out/('owned-cleanup-'+label+'-'+str(index)+'.stdout')
        stderr=out/('owned-cleanup-'+label+'-'+str(index)+'.stderr')
        argv=['/bin/ps','-axo','pid=,pgid=,uid=,comm=']
        record={'stage':stage,'argv':argv,'pgid':state['pgid'],'leaderBefore':child.poll(),
            'startedMonotonic':time.monotonic(),'complete':False,'members':[],'exit':None,'failure':None}
        state['inventories'].append(record)
        try:
            with stdout.open('xb') as output,stderr.open('xb') as diagnostics:
                result=subprocess.run(argv,stdin=subprocess.DEVNULL,stdout=output,stderr=diagnostics,
                    timeout=5,preexec_fn=inventory_file_limit,env={**os.environ,'LC_ALL':'C'})
            record['exit']=result.returncode
            data=stdout.read_bytes();notes=stderr.read_bytes()
            assert result.returncode==0 and not notes and len(data)<2*1024*1024,'Incomplete bounded current inventory'
            lines=data.decode('utf-8',errors='strict').splitlines()
            assert 0<len(lines)<=16384,'Inventory row bound'
            rows=[]
            for line in lines:
                fields=line.split(None,3)
                assert len(fields)==4 and len(fields[3])<=4096,'Malformed current inventory row'
                pid,pgid,uid=map(int,fields[:3])
                assert pid>=0 and pgid>=0 and -2<=uid<=0xffffffff
                rows.append({'pid':pid,'pgid':pgid,'uid':uid,'comm':fields[3]})
            assert len({row['pid'] for row in rows})==len(rows),'Duplicate current PID'
            record['members']=[row for row in rows if row['pgid']==state['pgid']]
            assert len(record['members'])<=256,'Current group member bound'
            record['rowCount']=len(rows);record['complete']=True
        except BaseException as error:record['failure']=type(error).__name__+': '+str(error)
        finally:
            record['leaderAfter']=child.poll();state['leaderReturnCode']=child.returncode
            record['finishedMonotonic']=time.monotonic()
            record['stdoutSHA256']=sha(stdout) if stdout.is_file() else None
            record['stderrSHA256']=sha(stderr) if stderr.is_file() else None
        return record

    def absent(snapshot):
        if snapshot['leaderBefore'] is not None and snapshot['leaderAfter'] is not None and child.returncode is not None and snapshot['complete'] and not snapshot['members']:
            state['stopped']=True;state['absenceSnapshot']=len(state['inventories'])
            state['absenceMethod']='REAPED_LEADER_AND_COMPLETE_CURRENT_PGID_INVENTORY_EMPTY'
            return True
        return False

    def current_signal(number):
        event={'signal':number,'pgid':state['pgid'],'leaderReturnCode':child.poll(),'sent':False,'failure':None}
        state['signals'].append(event)
        try:
            assert child.returncode is None,'Leader reaped; historical PGID signalling refused'
            assert os.getpgid(child.pid)==state['pgid'],'Current owned leader changed process group'
            event['ownership']='CURRENT_UNREAPED_OWNED_LEADER_PGID'
            os.killpg(state['pgid'],number);event['sent']=True
        except BaseException as error:
            event['failure']=type(error).__name__+': '+str(error);event['errno']=getattr(error,'errno',None)
            event['permissionErrorMeansAbsent']=False
        return event['sent']

    try:
        snapshot=inventory('retirement-entry')
        if absent(snapshot):return True
        for number,seconds,stage in [(signal.SIGTERM,2,'after-current-term'),(signal.SIGKILL,10,'after-current-kill')]:
            if child.poll() is not None:break
            if not current_signal(number):break
            try:child.wait(timeout=seconds)
            except subprocess.TimeoutExpired:pass
            state['leaderReturnCode']=child.returncode
            snapshot=inventory(stage)
            if absent(snapshot):return True
        child.poll()
        if child.returncode is not None and len(state['inventories'])<5:
            time.sleep(.2)
            snapshot=inventory('final-after-reap')
            if absent(snapshot):return True
        state['unknownReason']='No complete empty current group inventory begun after owned leader reaping'
    except BaseException as error:state['unknownReason']=type(error).__name__+': '+str(error)
    finally:state['leaderReturnCode']=child.poll()
    return False
