"""Trusted preview assets, HLS restrictions and mandatory SOCKS transport."""

import importlib.util
from pathlib import Path
import sys
import socket
import threading
import time
import unittest
from unittest import mock
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from tv_preview_policy import checked_manifest, media_target, preview_manifest, MAX_MANIFEST
import tv_preview_transport as TRANSPORT

SPEC = importlib.util.spec_from_file_location("preview", ROOT / "scripts/tv-preview.py")
PREVIEW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREVIEW)
PREFIX = "012345abcdef"
MASTER = b'#EXTM3U\n#EXT-X-VERSION:9\n#EXT-X-STREAM-INF:BANDWIDTH=1100000,CODECS="avc1.640029,mp4a.40.2"\nstream.m3u8\n'
MEDIA = (f'#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI="{PREFIX}_init2.mp4"\n'
         f'#EXT-X-GAP\n#EXTINF:1,\ngap.mp4\n#EXT-X-PART:DURATION=0.2,URI="{PREFIX}_part7.mp4",INDEPENDENT=YES\n'
         f'#EXTINF:1.0,\n{PREFIX}_seg7.mp4\n#EXT-X-PRELOAD-HINT:TYPE=PART,URI="{PREFIX}_part8.mp4"\n').encode()


class Policy(unittest.TestCase):
    def test_own_master_parts_codec_epochs_and_gaps(self):
        for data in [MASTER, MEDIA]:
            self.assertEqual(checked_manifest(data), data)
        for path in ['/tv/live/event-tv/index.m3u8', '/tv/live/event-tv/stream.m3u8?_HLS_msn=99&_HLS_part=2',
                     f'/tv/live/event-tv/{PREFIX}_init2.mp4', '/tv/live/event-tv/gap.mp4']:
            self.assertTrue(media_target(path), path)

    def test_preview_uses_complete_segments_without_changing_public_source(self):
        result = preview_manifest(MEDIA)
        self.assertIn(b'#EXT-X-START:TIME-OFFSET=-6.0,PRECISE=NO', result)
        self.assertIn(b'#EXT-X-MAP:', result)
        self.assertIn(b'gap.mp4', result)
        self.assertNotIn(b'#EXT-X-PART:', result)
        self.assertNotIn(b'#EXT-X-PRELOAD-HINT:', result)
        self.assertEqual(preview_manifest(MASTER), MASTER)
        self.assertEqual(checked_manifest(MEDIA), MEDIA)

    def test_external_urls_and_active_playlist_features_are_refused(self):
        values = [b'#EXTM3U\nhttps://evil.example/live.m3u8\n', b'#EXTM3U\n//evil.example/seg.mp4\n',
                  b'#EXTM3U\nfile:///private/key\n', b'#EXTM3U\n../other.mp4\n',
                  b'#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="https://evil.example/key"\n',
                  b'#EXTM3U\n#EXT-X-CONTENT-STEERING:SERVER-URI="https://evil.example/"\n',
                  b'#EXTM3U\n#EXT-X-MEDIA:URI="stream.m3u8"\n',
                  b'#EXTM3U\n#EXT-X-DEFINE:NAME="host",VALUE="evil.example"\n',
                  f'#EXTM3U\n#EXT-X-PART:URI="{PREFIX}_part1.mp4",URI="https://evil.example/"\n'.encode(),
                  b'#EXTM3U\n#EXT-X-PART:URI = "https://evil.example/"\n',
                  b'#EXTM3U\n#EXT-X-MAP:URI="data:text/plain,private"\n', b'#EXTM3U\r\n', b'\xff']
        for value in values:
            with self.subTest(value=value[:80]), self.assertRaises((ValueError, UnicodeError)):
                checked_manifest(value)

    def test_budgets_routes_queries_and_credentials(self):
        with self.assertRaises(ValueError):
            checked_manifest(b'#EXTM3U\n' + b' ' * MAX_MANIFEST)
        for path in ['/tv/admin', '/tv/live/other/index.m3u8', '/tv/live/event-tv/../index.m3u8',
                     '//elsewhere/tv/live/event-tv/index.m3u8', '/tv/live/event-tv/index.m3u8?url=https://evil.example',
                     '/tv/live/event-tv/index.m3u8?_HLS_msn=1&_HLS_msn=2',
                     '/tv/live/event-tv/index.m3u8?_HLS_part=%0a', '/tv/live/event-tv/index.m3u8#fragment']:
            self.assertFalse(media_target(path), path)
        for origin in ['http://media.example', 'https://user:pass@media.example', 'https://media.example/other']:
            with self.assertRaises(Exception):
                PREVIEW.receiver_origin(origin)


class CurlTransport(unittest.TestCase):
    def setUp(self):
        self.handles=[]
        self.fake=mock.Mock()
        for key in ['PROXY','PROXYTYPE','NOPROXY','FOLLOWLOCATION','SSL_VERIFYPEER','SSL_VERIFYHOST',
                    'PROTOCOLS','PROTO_HTTPS','NOSIGNAL','CONNECTTIMEOUT','TIMEOUT','USERAGENT',
                    'NOPROGRESS','CAINFO','XFERINFOFUNCTION','URL','WRITEFUNCTION','RESPONSE_CODE','NUM_CONNECTS']:
            setattr(self.fake,key,key)
        def factory():
            outer=self
            class Handle:
                def __init__(self):self.options={};self.code=200;self.body=MASTER;self.failed=False;self.closed=False
                def setopt(self,key,value):self.options[key]=value
                def perform(self):
                    if self.failed:raise outer.fake.error('proxy offline')
                    if self.options['WRITEFUNCTION'](self.body) != len(self.body):raise outer.fake.error('body budget exceeded')
                def getinfo(self,key):return self.code if key=='RESPONSE_CODE' else 1
                def close(self):self.closed=True
            h=Handle();self.handles.append(h);return h
        self.fake.Curl.side_effect=factory
        self.fake.error=type('CurlError',(Exception,),{})
        self.patch=mock.patch.object(TRANSPORT,'pycurl',self.fake);self.patch.start();self.addCleanup(self.patch.stop)
        self.transport=TRANSPORT.TorTransport('https://media.example',9050,size=1)
        self.addCleanup(self.transport.close)

    def test_every_connection_requires_remote_dns_proxy_and_tls(self):
        self.assertEqual(self.transport.fetch('/tv/live/event-tv/index.m3u8')[0],200)
        h=self.handles[0]
        self.assertEqual(h.options['PROXY'],'127.0.0.1:9050')
        self.assertEqual(h.options['PROXYTYPE'],self.fake.PROXYTYPE_SOCKS5_HOSTNAME)
        self.assertEqual(h.options['NOPROXY'],'')
        self.assertEqual(h.options['SSL_VERIFYHOST'],2)
        self.assertEqual(h.options['SSL_VERIFYPEER'],1)
        self.assertEqual(h.options['FOLLOWLOCATION'],0)
        self.transport.fetch('/tv/live/event-tv/stream.m3u8')
        self.assertEqual(len(self.handles),1,'the same connection handle is reused')

    def test_redirects_are_not_returned_to_the_browser(self):
        self.handles[0].code=302
        with self.assertRaises(ValueError):self.transport.fetch('/tv/live/event-tv/index.m3u8')

    def test_failed_proxy_never_creates_a_direct_handle(self):
        self.handles[0].failed=True
        with self.assertRaises(ValueError):self.transport.fetch('/tv/live/event-tv/index.m3u8')
        self.assertEqual(len(self.handles),1)
        self.assertEqual(self.handles[0].options['PROXYTYPE'],self.fake.PROXYTYPE_SOCKS5_HOSTNAME)

    def test_untrusted_manifest_is_refused(self):
        self.handles[0].body=b'#EXTM3U\nhttps://evil.example/seg.mp4\n'
        with self.assertRaises(ValueError):self.transport.fetch('/tv/live/event-tv/index.m3u8')

    def test_oversized_manifest_and_media_are_rejected(self):
        for target, body in [('/tv/live/event-tv/index.m3u8', b'A'*(MAX_MANIFEST+1)),
                             (f'/tv/live/event-tv/{PREFIX}_seg1.mp4', b'A'*(8*1024*1024+1))]:
            self.handles[0].body=body
            with self.assertRaises(ValueError):self.transport.fetch(target)

    def test_stop_aborts_fetches_and_closes_handles(self):
        self.transport.stop()
        self.assertEqual(self.handles[0].options['XFERINFOFUNCTION'](0,0,0,0),1)
        with self.assertRaises(ValueError):self.transport.fetch('/tv/live/event-tv/index.m3u8')
        self.transport.close();self.assertTrue(self.handles[0].closed)


@unittest.skipIf(TRANSPORT.pycurl is None, 'install the pinned preview requirements for real proxy checks')
class RealProxy(unittest.TestCase):
    def test_socks_receives_the_hostname_and_never_bypasses_no_proxy(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(5)
            observed=[]
            def proxy():
                with listener.accept()[0] as client:
                    client.settimeout(5)
                    def take(size):
                        data=b''
                        while len(data)<size:
                            chunk=client.recv(size-len(data))
                            if not chunk:raise AssertionError('short SOCKS message')
                            data+=chunk
                        return data
                    greeting=take(2);take(greeting[1]);client.sendall(b'\x05\x00')
                    header=take(4)
                    self.assertEqual(header,b'\x05\x01\x00\x03')
                    observed.append(take(take(1)[0]).decode('ascii'));take(2)
                    client.sendall(b'\x05\x04\x00\x01'+b'\x00'*6)
            worker=threading.Thread(target=proxy);worker.start()
            transport=TRANSPORT.TorTransport('https://receiver.invalid',listener.getsockname()[1],size=1)
            try:
                with mock.patch.dict('os.environ',{'NO_PROXY':'*','no_proxy':'*'}):
                    with self.assertRaises(ValueError):transport.fetch('/tv/live/event-tv/index.m3u8')
            finally:transport.close();worker.join(6)
            self.assertFalse(worker.is_alive());self.assertEqual(observed,['receiver.invalid'])

    def test_dead_proxy_does_not_connect_to_the_origin(self):
        with socket.socket() as origin, socket.socket() as proxy:
            origin.bind(('127.0.0.1',0));origin.listen();origin.settimeout(.3)
            proxy.bind(('127.0.0.1',0));proxy_port=proxy.getsockname()[1];proxy.close()
            transport=TRANSPORT.TorTransport(f'https://127.0.0.1:{origin.getsockname()[1]}',proxy_port,size=1)
            try:
                with self.assertRaises(ValueError):transport.fetch('/tv/live/event-tv/index.m3u8')
                with self.assertRaises(TimeoutError):origin.accept()
            finally:transport.close()


class LoopbackServer(unittest.TestCase):
    def setUp(self):
        self.server = PREVIEW.PreviewServer(('127.0.0.1', 0), PREVIEW.PreviewHandler)
        self.server.assets = ROOT/'web/tv'
        self.server.transport=mock.Mock()
        self.server.slots = threading.BoundedSemaphore(4)
        self.worker = threading.Thread(target=self.server.serve_forever)
        self.worker.start()
        self.addCleanup(self.worker.join, 3)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f'http://127.0.0.1:{self.server.server_port}'

    def test_idle_browser_socket_cannot_hold_shutdown_open(self):
        # Shorten only the timeout for this real socket regression.
        with mock.patch.object(PREVIEW.PreviewHandler, 'timeout', .15):
            idle=socket.create_connection(('127.0.0.1', self.server.server_port),timeout=2)
            try:
                # A request sent after the idle deadline must find the socket
                # closed; server_close must then join the finished handler.
                time.sleep(.3)
                self.assertEqual(idle.recv(1), b'')
            finally:idle.close()

    def test_page_is_local_and_no_media_or_external_assets_are_fetched(self):
        with mock.patch.object(self.server.transport, 'fetch') as network:
            with urllib.request.urlopen(self.base+'/tv/') as response:
                text=response.read().decode()
                self.assertIn('ForgeSworn TV', text)
                self.assertIn("connect-src 'self'", response.headers['Content-Security-Policy'])
                self.assertEqual(response.headers['Referrer-Policy'], 'no-referrer')
                self.assertNotIn('http', text)
            network.assert_not_called()

    def test_cross_origin_and_rebound_host_requests_are_rejected(self):
        for headers in [{'Host':'evil.example'}, {'Origin':'https://evil.example'}, {'Sec-Fetch-Site':'cross-site'}]:
            with self.subTest(headers=headers), self.assertRaises(urllib.error.HTTPError) as failure:
                urllib.request.urlopen(urllib.request.Request(self.base+'/tv/', headers=headers))
            self.assertEqual(failure.exception.code, 403)
            failure.exception.close()

    def test_busy_or_failed_transport_is_503_with_no_redirect(self):
        with mock.patch.object(self.server.transport, 'fetch', side_effect=ValueError('proxy offline')):
            with self.assertRaises(urllib.error.HTTPError) as failure:
                urllib.request.urlopen(self.base+'/tv/live/event-tv/index.m3u8')
            self.assertEqual(failure.exception.code, 503)
            self.assertIsNone(failure.exception.headers.get('Location'))
            failure.exception.close()

    def test_busy_preview_starts_no_additional_transport(self):
        for _ in range(4):
            self.server.slots.acquire()
        try:
            with mock.patch.object(self.server.transport, 'fetch') as network:
                with self.assertRaises(urllib.error.HTTPError) as failure:
                    urllib.request.urlopen(self.base+'/tv/live/event-tv/index.m3u8')
                self.assertEqual(failure.exception.code, 503)
                failure.exception.close()
                network.assert_not_called()
        finally:
            for _ in range(4):
                self.server.slots.release()


if __name__ == '__main__':
    unittest.main()
