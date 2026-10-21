#!/usr/bin/env python3
"""Independent encrypted SRT executable qualification (no reference source).

Build sapflow-srt examples send/receive, then run from repository root. Uses
public lab secrets only. CSPRNG keys are never printed. Tests both directions,
AES128/256, rotations and loss; leaves no children/sockets/scratch behind.
Loopback evidence does not establish public-network/mobile/security readiness.
"""
import hashlib, heapq, json, os, pathlib, random, socket, subprocess, tempfile, threading, time
ROOT=pathlib.Path(__file__).resolve().parents[3]
SECRET='sapflow-lab-secret-2026'

def ephemeral():
    s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(('127.0.0.1',0));return s

def stop(child):
    if child is not None and child.poll() is None:
        child.terminate()
        try:child.wait(timeout=2)
        except subprocess.TimeoutExpired:child.kill();child.wait(timeout=2)

def case(tmp,source,bits,direction,impaired=False,negative=None):
    reserve=ephemeral();receiver_addr=reserve.getsockname();reserve.close()
    proxy=ephemeral();proxy.settimeout(.002);stop_event=threading.Event()
    rng=random.Random(bits+int(impaired));stats={'dropped_data':0,'dropped_ack':0,'key_flags':0,'km_requests':0,'data_packets':0,'rejection_codes':[]}
    errors=[]
    def relay():
        sender=None;pending=[];serial=0
        try:
            while not stop_event.is_set():
                now=time.monotonic()
                while pending and pending[0][0]<=now:
                    _,_,data,target=heapq.heappop(pending);proxy.sendto(data,target)
                try:data,address=proxy.recvfrom(2048)
                except socket.timeout:continue
                if address==receiver_addr:
                    if len(data)>=64 and data[:4]==b'\x80\x00\x00\x00':
                        code=int.from_bytes(data[36:40],'big')
                        if 1000<=code<3000:stats['rejection_codes'].append(code)
                    if sender is None:continue
                    if impaired and data[:2]==b'\x80\x02' and rng.random()<.1:stats['dropped_ack']+=1;continue
                    proxy.sendto(data,sender)
                else:
                    sender=address
                    if data[:4]==b'\xff\xff\x00\x03':stats['km_requests']+=1
                    if len(data)>=16 and not data[0]&128:
                        stats['data_packets']+=1
                        stats['key_flags']|=(data[4]>>3)&3
                        if impaired and rng.random()<.1:stats['dropped_data']+=1;continue
                        serial+=1;heapq.heappush(pending,(now+rng.uniform(0,.02) if impaired else now,serial,data,receiver_addr))
                    else:proxy.sendto(data,receiver_addr)
        except Exception as error:errors.append(repr(error))
    thread=threading.Thread(target=relay);thread.start()
    name=f'{bits}-{direction}-{impaired}-{negative}'
    out=tmp/(name+'.ts');log=tmp/(name+'.log')
    env=dict(os.environ,SAPFLOW_SRT_PASSPHRASE=SECRET,SAPFLOW_SRT_KEY_BITS=str(bits),SAPFLOW_SRT_REFRESH='32')
    latency=2000 if impaired else 300
    receiver=sender=None
    try:
        with out.open('wb') as output, log.open('wb') as stderr:
            if direction=='own-to-reference':
                receiver=subprocess.Popen(['srt-live-transmit','-a:no','-t:12',f'srt://127.0.0.1:{receiver_addr[1]}?mode=listener&latency={latency}&passphrase={SECRET}&pbkeylen={bits//8}&enforcedencryption=1','file://con'],stdout=output,stderr=stderr)
                time.sleep(.15)
                if negative=='wrong':env['SAPFLOW_SRT_PASSPHRASE']='wrong-lab-secret'
                if negative=='missing':env.pop('SAPFLOW_SRT_PASSPHRASE')
                run=subprocess.run([str(ROOT/'target/debug/examples/send'),f'127.0.0.1:{proxy.getsockname()[1]}','publish:crypto-lab',str(source),'50000',str(latency)],env=env,capture_output=True,timeout=15)
                time.sleep(.2);stop(receiver)
                sender_code=run.returncode;sender_log=run.stderr.decode()
            else:
                receiver=subprocess.Popen([str(ROOT/'target/debug/examples/receive'),f'127.0.0.1:{receiver_addr[1]}',str(out),str(source.stat().st_size),str(latency)],env=env,stdout=subprocess.PIPE,stderr=stderr)
                ready=receiver.stdout.readline();assert b'listening' in ready,ready
                secret='wrong-lab-secret' if negative=='wrong' else SECRET
                crypto='' if negative=='missing' else f'&passphrase={secret}&pbkeylen={bits//8}'
                sender=subprocess.Popen(['srt-live-transmit','-a:no','-t:12','-c:1316','file://con',f'srt://127.0.0.1:{proxy.getsockname()[1]}?mode=caller&latency={latency}&streamid=publish:crypto-lab&kmrefreshrate=32&kmpreannounce=8{crypto}'],stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
                try:
                    data=source.read_bytes()
                    for pos in range(0,len(data),1316):
                        sender.stdin.write(data[pos:pos+1316]);sender.stdin.flush();time.sleep(.025)
                except BrokenPipeError:pass
                try:sender.stdin.close()
                except BrokenPipeError:pass
                sender.stdin=None
                _,sender_stderr=sender.communicate(timeout=15);sender_code=sender.returncode;sender_log=sender_stderr.decode()
                if not negative:
                    try:receiver.wait(timeout=6)
                    except subprocess.TimeoutExpired:pass
                stop(receiver)
                receiver_log=receiver.stdout.read().decode()
            received=out.read_bytes();original=source.read_bytes()
            result=dict(direction=direction,bits=bits,profile='impaired' if impaired else 'clean',negative=negative,received_bytes=len(received),equal=received==original,prefix_equal=original.startswith(received),sha256=hashlib.sha256(received).hexdigest(),sender_code=sender_code,**stats)
            print(json.dumps(result),flush=True)
            if errors or (negative and (received or stats["data_packets"] or (direction=="own-to-reference" and sender_code==0) or (direction=="reference-to-own" and (1010 if negative=="wrong" else 1011) not in stats["rejection_codes"]))) or (not negative and (received!=original or stats['key_flags']!=3 or not stats['km_requests'])):
                print('sender:',sender_log,'receiver:',log.read_text(),locals().get('receiver_log',''),errors,flush=True)
                raise AssertionError('crypto interop failed')
    finally:
        stop(sender);stop(receiver);stop_event.set();thread.join(timeout=2);proxy.close()

def main():
    with tempfile.TemporaryDirectory(prefix='srt-crypto-interop-',dir=ROOT/'target') as scratch:
        tmp=pathlib.Path(scratch);source=tmp/'input.ts'
        subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=size=160x90:rate=15','-t','4','-c:v','libx264','-preset','ultrafast','-g','15','-f','mpegts',str(source)],check=True)
        # srt-live-transmit's stdin adapter discards a final partial chunk
        # at EOF (observed). Pad with legal 188-byte TS null packets so both
        # executables are given complete 1316-byte messages.
        null_packet=bytes.fromhex('471fff10')+b'\xff'*184
        with source.open('ab') as f:
            while source.stat().st_size%1316:f.write(null_packet);f.flush()
        for bits in (() if os.environ.get("SRT_NEGATIVE_ONLY") else (128,256)):
            for direction in ('own-to-reference','reference-to-own'):
                if os.environ.get('SRT_CASE') and os.environ['SRT_CASE']!=f'{bits}-{direction}':continue
                case(tmp,source,bits,direction)
                case(tmp,source,bits,direction,True)
        if os.environ.get('SRT_CASE'):return
        for direction in ('own-to-reference','reference-to-own'):
            for negative in ('wrong','missing'):case(tmp,source,128,direction,negative=negative)
if __name__=='__main__':main()
