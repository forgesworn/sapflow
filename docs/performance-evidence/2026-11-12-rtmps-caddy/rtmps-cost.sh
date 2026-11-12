#!/bin/bash
# RTMP direct against RTMPS through Caddy layer4 (TLS + PROXY v2) into one
# sapflow, N publishers of the v4v 720p clip: CPU of sapflow and of Caddy over
# a steady window, and whether every stream advances. Usage: rtmps-cost.sh N rtmp|rtmps LABEL
set -uo pipefail
export PATH=/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$PATH
N=$1; MODE=$2; LABEL=$3; S=$HOME/cap-linux; OUT=$S/out-rtmps/$LABEL; mkdir -p $OUT
NET=caprtmps; CADDY_IP=172.31.0.10
docker rm -f cap-srv cap-caddy cap-pub >/dev/null 2>&1
docker network inspect $NET >/dev/null 2>&1 || docker network create --subnet 172.31.0.0/24 $NET >/dev/null
docker run -d --name cap-srv --network $NET --cpus 1 --memory 1g --memory-swap 1g --pids-limit 128 \
  --ulimit nofile=65536:65536 -v $S/sapflow-loss:/usr/local/bin/sapflow:ro \
  -e SAPFLOW_OPEN_PUBLISH=1 -e SAPFLOW_HTTP_ADDR=0.0.0.0:8888 -e SAPFLOW_RTMP_ADDR=0.0.0.0:1935 \
  -e SAPFLOW_RTMP_PROXY_ADDR=0.0.0.0:1936 -e SAPFLOW_RTMP_TRUSTED_PROXY=$CADDY_IP \
  -e SAPFLOW_SEGMENT_COUNT=24 -e SAPFLOW_HLS_MODE=whole -e SAPFLOW_HEALTH_INTERVAL=0 -e RUST_LOG=info \
  sapflow-cost-linux:local /usr/local/bin/sapflow >/dev/null
docker run -d --name cap-caddy --network $NET --ip $CADDY_IP -v $S/caddy/caddy:/usr/local/bin/caddy:ro \
  -v $S/s/rtmps/Caddyfile:/etc/caddy/Caddyfile:ro -v $S/caddy/certs:/certs:ro \
  sapflow-cost-linux:local /usr/local/bin/caddy run --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null
sleep 3
if [ "$MODE" = rtmps ]; then url="rtmps://ingest.test:443/live"; else url="rtmp://cap-srv:1935/live"; fi
docker run -d --name cap-pub --network $NET --add-host ingest.test:$CADDY_IP -v $S/clips:/clips:ro -v $S/ffmpeg:/ff:ro -v $S/s/rtmps:/s:ro \
  sapflow-cost-linux:local bash -c "for i in \$(seq 1 $N); do /ff/bin/ffmpeg -hide_banner -loglevel error -nostdin -re -stream_loop -1 -i /clips/v4v720.flv -c copy -f flv $url/event-cost-\$i > /dev/null 2>&1 & sleep 0.5; done; wait" >/dev/null
cpu() { docker exec $1 sh -c 'awk "\$1==\"usage_usec\"{print \$2}" /sys/fs/cgroup/cpu.stat'; }
ready=0; for _ in $(seq 120); do
  ready=$(docker exec cap-pub python3 /s/count-ready.py $N 2>/dev/null || echo 0)
  [ "$ready" = "$N" ] && break; sleep 1; done
sleep 20
s0=$(cpu cap-srv); c0=$(cpu cap-caddy); t0=$(date +%s.%N)
sleep 60
s1=$(cpu cap-srv); c1=$(cpu cap-caddy); t1=$(date +%s.%N)
adv=$(docker exec cap-pub python3 /s/count-ready.py $N)
python3 - "$N" "$MODE" "$ready" "$adv" "$s0" "$s1" "$c0" "$c1" "$t0" "$t1" <<'PY' | tee $OUT/result.txt
import sys
n, mode, ready, adv = sys.argv[1:5]
s0, s1, c0, c1 = map(int, sys.argv[5:9]); t0, t1 = map(float, sys.argv[9:11])
w = t1 - t0
srv = 100 * (s1 - s0) / 1e6 / w; cad = 100 * (c1 - c0) / 1e6 / w
print(f"n={n} mode={mode} ready={ready} listed_at_end={adv} sapflow_cpu={srv:.1f}% caddy_cpu={cad:.1f}% per_stream sapflow={srv/int(n):.2f}% caddy={cad/int(n):.2f}% window={w:.0f}s")
PY
docker logs cap-srv 2>&1 | grep -c -E "PROXY protocol|connection ended" | sed 's/^/sapflow connection-end lines: /'
docker rm -f cap-pub cap-caddy cap-srv >/dev/null 2>&1
