#!/usr/bin/env python3
"""A TCP relay for the OBS reconnect test: listen LISTEN, forward to UPSTREAM.

On SIGUSR1 every open pair is cut the way a venue network that goes cuts
it: OBS's side is reset at once (SO_LINGER 0), so OBS sees the loss and
reconnects, while sapflow's side stays open and receives nothing more, so
sapflow still holds the old connection until it times out. New connections
are relayed as usual. Logs one line per event with a monotonic timestamp.
Dependency-free; loopback only.

    python3 scripts/obs-cut-relay.py 47412 47402 &
    node scripts/obs-ingest.js --bin <sapflow> --out <dir> --protocol rtmp \
        --obs-rtmp-port 47412 --obs-keyint 1 --seconds 60 &
    sleep 32; kill -USR1 <relay pid>   # the venue network goes
"""
import asyncio, signal, socket, struct, sys, time

LISTEN, UPSTREAM = int(sys.argv[1]), int(sys.argv[2])
T0 = time.monotonic()
pairs = []  # [client_writer, upstream_writer, alive_flag(list)]


def log(msg):
    print(f"{time.monotonic() - T0:8.3f} {msg}", flush=True)


async def pump(reader, writer, alive):
    try:
        while alive[0]:
            data = await reader.read(65536)
            if not data or not alive[0]:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        pass


async def handle(creader, cwriter):
    n = len(pairs) + 1
    try:
        ureader, uwriter = await asyncio.open_connection("127.0.0.1", UPSTREAM)
    except OSError as e:
        log(f"pair {n}: upstream refused ({e})")
        cwriter.close()
        return
    alive = [True]
    pairs.append((cwriter, uwriter, alive))
    log(f"pair {n}: OBS connected")
    await asyncio.gather(pump(creader, uwriter, alive), pump(ureader, cwriter, alive))
    if alive[0]:
        alive[0] = False
        log(f"pair {n}: ended by an endpoint")
        for w in (cwriter, uwriter):
            try:
                w.close()
            except Exception:
                pass


def cut():
    for i, (cwriter, uwriter, alive) in enumerate(pairs, 1):
        if not alive[0]:
            continue
        alive[0] = False
        sock = cwriter.get_extra_info("socket")
        try:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        except OSError:
            pass
        cwriter.transport.abort()  # RST to OBS; sapflow's side left open and silent
        log(f"pair {i}: cut (OBS reset, sapflow side left silent)")


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", LISTEN)
    asyncio.get_running_loop().add_signal_handler(signal.SIGUSR1, cut)
    log(f"relaying {LISTEN} -> {UPSTREAM}")
    async with server:
        await server.serve_forever()


asyncio.run(main())
