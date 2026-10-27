#!/usr/bin/env python3
"""Real Sapflow -> Chromium decoding gate on loopback; no Tor claim.

Requires the CI smoke build, FFmpeg and a Playwright module supplied through
SAPFLOW_PLAYWRIGHT_MODULE. All source media and process state are disposable.
"""
import importlib.util
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import urllib.error

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts'))
spec=importlib.util.spec_from_file_location('preview',ROOT/'scripts/tv-preview.py')
preview=importlib.util.module_from_spec(spec);spec.loader.exec_module(preview)


def port(kind=socket.SOCK_STREAM):
    with socket.socket(type=kind) as candidate:
        candidate.bind(('127.0.0.1',0));return candidate.getsockname()[1]


def stop(child):
    if child is None:return
    if child.poll() is None:
        os.killpg(child.pid,15)
        try:child.wait(timeout=10)
        except subprocess.TimeoutExpired:os.killpg(child.pid,9);child.wait()


def main():
    binary=Path(os.environ.get('SAPFLOW_BIN',ROOT/'target/debug/sapflow')).resolve()
    if not binary.is_file():raise SystemExit('Build Sapflow before the TV browser gate')
    output=ROOT/'target/tv-browser';output.mkdir(parents=True,exist_ok=True)
    http,rtmp,srt=port(),port(),port(socket.SOCK_DGRAM)
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    engine=publisher=server=worker=None
    with tempfile.TemporaryDirectory(prefix='sapflow-tv-browser-') as work:
        home=Path(work);media=home/'media';media.mkdir()
        # Distinct tracks deliberately contain audio tails shorter than video.
        # The continuous playout clock must fill those gaps across every cut.
        for index, (colour, duration, audio_duration) in enumerate([('red',4,2),('lime',5,4),('blue',6,6)]):
            subprocess.run(['ffmpeg','-v','error','-f','lavfi','-i',f'color=c={colour}:s=320x180:r=25:d={duration}',
                            '-f','lavfi','-i',f'sine=r=48000:d={audio_duration}:f={440+index*220}',
                            '-c:v','libx264','-preset','medium','-g','25','-keyint_min','25','-sc_threshold','0',
                            '-pix_fmt','yuv420p','-c:a','aac','-ac','2',str(media/f'{index:02}-generated.mp4')],check=True,timeout=60)
        with (output/'engine.log').open('w') as engine_log,(output/'publisher.log').open('w') as pub_log:
            try:
                env={k:v for k,v in os.environ.items() if not k.startswith('SAPFLOW_')}
                env.update(SAPFLOW_OPEN_PUBLISH='1',SAPFLOW_HTTP_ADDR=f'127.0.0.1:{http}',
                           SAPFLOW_RTMP_ADDR=f'127.0.0.1:{rtmp}',SAPFLOW_SRT_ADDR=f'127.0.0.1:{srt}',
                           SAPFLOW_HTTP_THREADS='1',SAPFLOW_RECONNECT_GRACE='10')
                engine=subprocess.Popen([str(binary)],env=env,stdout=engine_log,stderr=subprocess.STDOUT,start_new_session=True)
                deadline=time.monotonic()+10
                while True:
                    if engine.poll() is not None:raise RuntimeError('Fixture Sapflow failed; inspect engine.log')
                    try:
                        with socket.create_connection(('127.0.0.1',rtmp),timeout=.1):break
                    except OSError:
                        if time.monotonic()>deadline:raise
                        time.sleep(.1)
                publisher=subprocess.Popen(['bash',str(ROOT/'scripts/tv-playout.sh'),'--strip-metadata','--no-verify',str(media),
                                            f'rtmp://127.0.0.1:{rtmp}/live/event-tv'],stdout=pub_log,stderr=subprocess.STDOUT,start_new_session=True)
                deadline=time.monotonic()+20
                while True:
                    try:
                        data=opener.open(f'http://127.0.0.1:{http}/live/event-tv/stream.m3u8',timeout=2).read()
                        if data.count(b'#EXTINF:')>=20:break
                    except urllib.error.URLError:pass
                    if time.monotonic()>deadline:raise RuntimeError('Fixture live stream never primed')
                    time.sleep(.2)

                def fixture_fetch(target):
                    # Explicit test-only loopback transport. Production preview
                    # has no HTTP/direct mode and always requires the Tor transport.
                    try:
                        with opener.open(f'http://127.0.0.1:{http}'+target.removeprefix('/tv'),timeout=10) as r:
                            body=r.read(8*1024*1024+1)
                            if len(body)>8*1024*1024:raise ValueError('fixture response too large')
                            mime='application/vnd.apple.mpegurl' if '.m3u8' in target else 'video/mp4'
                            return r.status,body,mime
                    except urllib.error.HTTPError as error:
                        code=error.code;error.close();return code,b'Unavailable\n','text/plain'

                if os.environ.get('SAPFLOW_TV_TEST_PEERS') == '1':
                    subprocess.run(['node',str(ROOT/'tests/tv-peers-browser.mjs'),f'http://127.0.0.1:{http}/live/event-tv/',str(publisher.pid)],check=True,timeout=150)

                server=preview.PreviewServer(('127.0.0.1',0),preview.PreviewHandler)
                class FixtureTransport:
                    def fetch(self,target):
                        if target=='/tv/audience':
                            return 200,b'{"activePlayers":2,"peerEnabledPlayers":1,"updatedAt":1,"ttlSeconds":35,"playerVersion":"2026.10.27.7","channel":{"state":"live","startedAt":1,"endedAt":null,"lastMediaAgeSeconds":0}}', 'application/json'
                        return fixture_fetch(target)
                server.assets=ROOT/'web/tv';server.transport=FixtureTransport()
                server.slots=threading.BoundedSemaphore(4)
                worker=threading.Thread(target=server.serve_forever);worker.start()
                subprocess.run(['node',str(ROOT/'tests/tv-player-browser.mjs'),f'http://127.0.0.1:{server.server_port}/tv/',
                                str(output/'chromium'),'20','chrome'],check=True,timeout=150,
                               env=dict(os.environ,SAPFLOW_TV_TEST_CUTS='1'))
                subprocess.run(['node',str(ROOT/'tests/tv-player-browser.mjs'),f'http://127.0.0.1:{server.server_port}/tv/',
                                str(output/'managed'),'20','chrome-managed'],check=True,timeout=150,
                               env=dict(os.environ,SAPFLOW_TV_TEST_CUTS='1'))
                subprocess.run(['node',str(ROOT/'tests/tv-network-browser.mjs'),f'http://127.0.0.1:{server.server_port}/tv/'],check=True,timeout=180)
            finally:
                if server:
                    server.shutdown();server.server_close();worker.join(timeout=5)
                stop(publisher);stop(engine)


if __name__=='__main__':main()
