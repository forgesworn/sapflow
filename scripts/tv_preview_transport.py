"""Reused, bounded HTTPS connections through an explicitly selected Tor proxy."""
import queue
import ssl
import threading
from urllib.parse import urlsplit

try:
    import pycurl
except ImportError:
    pycurl = None

from tv_preview_policy import MAX_MANIFEST, preview_manifest


class TorTransport:
    def __init__(self, origin, socks_port, size=4):
        if pycurl is None:
            raise RuntimeError('Install scripts/requirements-tv-preview.txt in a private virtual environment')
        self.origin=origin
        self.pool=queue.LifoQueue(size)
        self.stopped=threading.Event()
        self.lock=threading.Lock()
        self.requests=self.connections=0
        for _ in range(size):
            handle=pycurl.Curl()
            for option,value in [(pycurl.PROXY,f'127.0.0.1:{socks_port}'),
                                 (pycurl.PROXYTYPE,pycurl.PROXYTYPE_SOCKS5_HOSTNAME),
                                 (pycurl.NOPROXY,''),(pycurl.FOLLOWLOCATION,0),
                                 (pycurl.SSL_VERIFYPEER,1),(pycurl.SSL_VERIFYHOST,2),
                                 (pycurl.PROTOCOLS,pycurl.PROTO_HTTPS),(pycurl.NOSIGNAL,1),
                                 (pycurl.CONNECTTIMEOUT,20),(pycurl.TIMEOUT,45),
                                 (pycurl.USERAGENT,'Sapflow-TV-preview/1'),(pycurl.NOPROGRESS,0)]:
                handle.setopt(option,value)
            cafile=ssl.get_default_verify_paths().cafile
            if cafile:handle.setopt(pycurl.CAINFO,cafile)
            handle.setopt(pycurl.XFERINFOFUNCTION,lambda *_:int(self.stopped.is_set()))
            self.pool.put(handle)

    def fetch(self,target):
        if self.stopped.is_set():raise ValueError('preview stopped')
        handle=self.pool.get_nowait()
        budget=MAX_MANIFEST if urlsplit(target).path.endswith('.m3u8') else 8*1024*1024
        body=bytearray()
        def write(chunk):
            if len(body)+len(chunk)>budget:return 0
            body.extend(chunk);return len(chunk)
        try:
            handle.setopt(pycurl.URL,self.origin+target)
            handle.setopt(pycurl.WRITEFUNCTION,write)
            handle.perform()
            code=handle.getinfo(pycurl.RESPONSE_CODE)
            with self.lock:
                self.requests+=1;self.connections+=handle.getinfo(pycurl.NUM_CONNECTS)
            if code not in (200,400,404,410,429,503):raise ValueError('receiver redirect or unexpected response')
            if code!=200:return code,b'Channel unavailable\n','text/plain; charset=utf-8'
            data=bytes(body)
            if urlsplit(target).path.endswith('.m3u8'):
                return 200,preview_manifest(data),'application/vnd.apple.mpegurl'
            return 200,data,'video/mp4'
        except pycurl.error as error:
            raise ValueError('Tor fetch failed') from error
        finally:
            handle.setopt(pycurl.WRITEFUNCTION,lambda chunk:len(chunk))
            self.pool.put(handle)

    def stop(self):self.stopped.set()

    def close(self):
        self.stop()
        while not self.pool.empty():self.pool.get_nowait().close()
