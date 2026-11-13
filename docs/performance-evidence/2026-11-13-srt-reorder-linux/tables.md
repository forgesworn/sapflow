# SRT reorder tolerance on Linux: per condition

Mean ± sample standard deviation over the seeds, per receiver: `own-old`
is main at 3e32000, `own-new` the reorder wait, `own-aa` main again (an
A/A control), `libsrt` srt-live-transmit 1.5.1. Overhead is `data_retx` /
`data_orig` from `results.tsv`; video delivered is against the same
receiver's clean run. `reordered`, `nak_held`, `dropped` and the largest
wait come from sapflow's close log (`sapflow-close-counters.tsv`); main
logs no `nak_held` or wait. Conditions: `none`, `reorder` (0 to 20 ms
jitter, reordering), `keep` (0 to 20 ms, order kept), `reorder100` (0 to
100 ms, reordering), and eight-seed reruns at 200 ms with bursty loss,
`reorder200` (20 ms reordering) and `none200aa` (no jitter, main twice).

### reorder

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 2000 | libsrt | 3/3 | 69.7 ± 0.6 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 2000 | own-new | 3/3 | 6.4 ± 1.3 | 100.0 ± 0.0 | 5649.7 ± 133.5 | 5563.3 ± 124.9 | 0.0 ± 0.0 | 40.8 ± 11.6 |
| burst:5,8 | 2000 | own-old | 3/3 | 65.1 ± 1.0 | 100.0 ± 0.0 | 5649.7 ± 65.8 | - | 0.0 ± 0.0 | - |
| random:2 | 2000 | libsrt | 3/3 | 69.4 ± 0.1 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 2000 | own-new | 3/3 | 3.1 ± 0.0 | 100.0 ± 0.0 | 5885.0 ± 29.5 | 5795.7 ± 33.7 | 0.0 ± 0.0 | 42.8 ± 6.3 |
| random:2 | 2000 | own-old | 3/3 | 66.5 ± 0.7 | 100.0 ± 0.0 | 5871.0 ± 11.1 | - | 0.0 ± 0.0 | - |
| burst:5,8 | 200 | libsrt | 3/3 | 69.9 ± 0.7 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 200 | own-new | 3/3 | 68.6 ± 1.2 | 96.1 ± 4.9 | 5649.7 ± 91.7 | 0.0 ± 0.0 | 11.3 ± 10.0 | 25.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 3/3 | 68.9 ± 0.7 | 95.3 ± 1.7 | 5615.3 ± 61.8 | - | 5.0 ± 3.6 | - |
| random:2 | 200 | libsrt | 3/3 | 69.2 ± 0.2 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 200 | own-new | 3/3 | 67.4 ± 0.4 | 100.0 ± 0.0 | 5852.3 ± 23.4 | 0.0 ± 0.0 | 0.0 ± 0.0 | 25.0 ± 0.0 |
| random:2 | 200 | own-old | 3/3 | 67.7 ± 0.1 | 100.0 ± 0.0 | 5846.3 ± 44.1 | - | 0.0 ± 0.0 | - |
clean libsrt 200 ms: video 895, overhead 69.0 ± 0.0, status ['FAIL']
clean libsrt 2000 ms: video 895, overhead 68.8 ± 0.0, status ['FAIL']
clean own-new 200 ms: video 878, overhead 65.7 ± 0.0, status ['ok']
clean own-new 2000 ms: video 878, overhead 1.0 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 64.7 ± 0.0, status ['ok']
clean own-old 2000 ms: video 878, overhead 65.5 ± 0.0, status ['ok']

### none

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 2000 | libsrt | 3/3 | 5.8 ± 1.1 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 2000 | own-new | 3/3 | 5.5 ± 1.4 | 100.8 ± 1.4 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |
| burst:5,8 | 2000 | own-old | 3/3 | 5.4 ± 1.3 | 100.0 ± 0.0 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
| random:2 | 2000 | libsrt | 3/3 | 2.0 ± 0.0 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 2000 | own-new | 3/3 | 2.0 ± 0.1 | 100.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |
| random:2 | 2000 | own-old | 3/3 | 2.0 ± 0.0 | 100.0 ± 0.0 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
| burst:5,8 | 200 | libsrt | 3/3 | 5.8 ± 1.3 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 200 | own-new | 3/3 | 6.4 ± 1.8 | 98.1 ± 1.5 | 0.0 ± 0.0 | 0.0 ± 0.0 | 9.3 ± 5.7 | 0.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 3/3 | 7.4 ± 1.3 | 100.0 ± 0.0 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
| random:2 | 200 | libsrt | 3/3 | 2.1 ± 0.0 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 200 | own-new | 3/3 | 2.6 ± 0.0 | 100.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |
| random:2 | 200 | own-old | 3/3 | 2.5 ± 0.2 | 100.8 ± 1.4 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
clean libsrt 200 ms: video 895, overhead 0.0 ± 0.0, status ['FAIL']
clean libsrt 2000 ms: video 895, overhead 0.0 ± 0.0, status ['FAIL']
clean own-new 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-new 2000 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-old 2000 ms: video 878, overhead 0.0 ± 0.0, status ['ok']

### keep

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 2000 | libsrt | 3/3 | 7.5 ± 1.6 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 2000 | own-new | 3/3 | 5.4 ± 1.3 | 100.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |
| burst:5,8 | 2000 | own-old | 3/3 | 5.3 ± 1.2 | 100.0 ± 0.0 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
| random:2 | 2000 | libsrt | 3/3 | 2.8 ± 0.0 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 2000 | own-new | 3/3 | 2.0 ± 0.1 | 100.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.0 ± 0.0 |
| random:2 | 2000 | own-old | 3/3 | 2.0 ± 0.1 | 100.0 ± 0.0 | 0.0 ± 0.0 | - | 0.0 ± 0.0 | - |
| burst:5,8 | 200 | libsrt | 3/3 | 7.4 ± 1.8 | 99.9 ± 0.1 | - | - | - | - |
| burst:5,8 | 200 | own-new | 3/3 | 9.1 ± 1.8 | 94.5 ± 2.5 | 0.0 ± 0.0 | 0.0 ± 0.0 | 13.0 ± 9.9 | 0.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 3/3 | 8.6 ± 2.0 | 94.6 ± 1.5 | 0.0 ± 0.0 | - | 14.0 ± 4.4 | - |
| random:2 | 200 | libsrt | 3/3 | 2.9 ± 0.0 | 100.1 ± 0.2 | - | - | - | - |
| random:2 | 200 | own-new | 3/3 | 3.4 ± 0.2 | 99.5 ± 0.9 | 0.0 ± 0.0 | 0.0 ± 0.0 | 0.3 ± 0.6 | 0.0 ± 0.0 |
| random:2 | 200 | own-old | 3/3 | 3.4 ± 0.2 | 99.1 ± 1.1 | 0.0 ± 0.0 | - | 0.5 ± 0.7 | - |
clean libsrt 200 ms: video 895, overhead 0.0 ± 0.0, status ['FAIL']
clean libsrt 2000 ms: video 895, overhead 0.0 ± 0.0, status ['FAIL']
clean own-new 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-new 2000 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-old 2000 ms: video 878, overhead 0.0 ± 0.0, status ['ok']

### reorder100

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 2000 | libsrt | 3/3 | 88.8 ± 2.1 | 100.0 ± 0.0 | - | - | - | - |
| burst:5,8 | 2000 | own-new | 3/3 | 26.8 ± 2.2 | 100.0 ± 0.0 | 6373.0 ± 91.5 | 3918.7 ± 137.0 | 0.0 ± 0.0 | 50.0 ± 0.0 |
| burst:5,8 | 2000 | own-old | 3/3 | 73.8 ± 0.4 | 100.0 ± 0.1 | 6278.7 ± 146.3 | - | 0.0 ± 0.0 | - |
| random:2 | 2000 | libsrt | 3/3 | 87.4 ± 0.5 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 2000 | own-new | 3/3 | 26.2 ± 1.3 | 99.8 ± 0.3 | 6557.0 ± 31.1 | 4070.0 ± 59.4 | 0.0 ± 0.0 | 50.0 ± 0.0 |
| random:2 | 2000 | own-old | 3/3 | 75.4 ± 0.7 | 100.0 ± 0.0 | 6553.0 ± 33.8 | - | 0.0 ± 0.0 | - |
| burst:5,8 | 200 | libsrt | 3/3 | 89.0 ± 1.8 | 99.7 ± 0.2 | - | - | - | - |
| burst:5,8 | 200 | own-new | 3/3 | 88.1 ± 3.2 | 45.4 ± 7.1 | 6338.7 ± 129.1 | 0.0 ± 0.0 | 193.7 ± 40.1 | 25.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 3/3 | 88.6 ± 0.6 | 43.3 ± 8.2 | 6284.0 ± 95.7 | - | 202.7 ± 62.8 | - |
| random:2 | 200 | libsrt | 3/3 | 87.5 ± 1.2 | 100.0 ± 0.0 | - | - | - | - |
| random:2 | 200 | own-new | 3/3 | 91.8 ± 0.9 | 43.1 ± 3.3 | 6567.0 ± 24.2 | 0.0 ± 0.0 | 53.7 ± 6.0 | 25.0 ± 0.0 |
| random:2 | 200 | own-old | 3/3 | 93.2 ± 1.4 | 43.6 ± 4.4 | 6573.0 ± 17.4 | - | 50.7 ± 4.0 | - |
clean libsrt 200 ms: video 895, overhead 86.2 ± 0.0, status ['FAIL']
clean libsrt 2000 ms: video 895, overhead 85.5 ± 0.0, status ['FAIL']
clean own-new 200 ms: video 878, overhead 91.1 ± 0.0, status ['ok']
clean own-new 2000 ms: video 878, overhead 18.1 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 89.3 ± 0.0, status ['ok']
clean own-old 2000 ms: video 878, overhead 73.6 ± 0.0, status ['ok']

### reorder200

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 200 | own-new | 8/8 | 70.7 ± 1.1 | 95.2 ± 2.4 | 5673.4 ± 40.7 | 0.0 ± 0.0 | 14.4 ± 8.2 | 25.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 8/8 | 70.4 ± 1.3 | 96.0 ± 1.4 | 5671.8 ± 63.6 | - | 10.1 ± 8.9 | - |
clean own-new 200 ms: video 878, overhead 65.4 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 65.1 ± 0.0, status ['ok']

### none200aa

| Loss | Latency | Receiver | n ok | Overhead % | Video delivered % | reordered | nak_held | dropped | max wait ms |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| burst:5,8 | 200 | own-aa | 8/8 | 7.1 ± 1.2 | 98.6 ± 1.7 | 0.0 ± 0.0 | - | 3.2 ± 4.4 | - |
| burst:5,8 | 200 | own-new | 8/8 | 7.3 ± 1.3 | 98.0 ± 2.1 | 0.0 ± 0.0 | 0.0 ± 0.0 | 4.6 ± 4.2 | 0.0 ± 0.0 |
| burst:5,8 | 200 | own-old | 8/8 | 7.2 ± 0.8 | 98.8 ± 1.4 | 0.0 ± 0.0 | - | 2.9 ± 3.3 | - |
clean own-aa 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-new 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
clean own-old 200 ms: video 878, overhead 0.0 ± 0.0, status ['ok']
