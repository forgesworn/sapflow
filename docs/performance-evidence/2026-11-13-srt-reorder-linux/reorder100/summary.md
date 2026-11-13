Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Unqualified trial: libsrt none 200 ms seed 0 (FAIL)
Encryption: aes256, no key refresh

WARNING: the proxy reordered packets in 42 trials; those numbers are suspect


Unqualified or incomplete trials: 2 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own-old | 3 | 75.4% ± 0.7 | 0.00% ± 0.00 | 100.00% ± 0.00 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| random:2 | 2000 ms | own-new | 3 | 26.2% ± 1.3 | 0.00% ± 0.00 | 99.81% ± 0.33 | 99.64% ± 0.62 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 87.4% ± 0.5 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-old | 3 | 73.8% ± 0.4 | 0.04% ± 0.08 | 99.96% ± 0.07 | 99.86% ± 0.25 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-new | 3 | 26.8% ± 2.2 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 88.8% ± 2.1 | 0.01% ± 0.01 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:2 | 200 ms | own-old | 3 | 93.2% ± 1.4 | 0.09% ± 0.05 | 43.58% ± 4.41 | 91.73% ± 9.80 | 0.0 ± 0.0 |
| random:2 | 200 ms | own-new | 3 | 91.8% ± 0.9 | 0.12% ± 0.03 | 43.13% ± 3.34 | 89.95% ± 7.24 | 0.0 ± 0.0 |
| random:2 | 200 ms | libsrt | 3 | 87.5% ± 1.2 | 0.10% ± 0.06 | 0.00% ± 0.00 | 0.00% ± 0.00 | 38.0 ± 8.7 |
| burst:5,8 | 200 ms | own-old | 3 | 88.6% ± 0.6 | 0.87% ± 0.37 | 43.32% ± 8.16 | 86.44% ± 2.32 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 3 | 88.1% ± 3.2 | 0.92% ± 0.19 | 45.37% ± 7.08 | 83.94% ± 10.98 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | libsrt | 3 | 89.0% ± 1.8 | 0.93% ± 0.52 | 0.00% ± 0.00 | 0.00% ± 0.00 | 75.7 ± 27.5 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own-old | -12.0 | -23.2 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 2000 ms | own-new | -61.2 | -74.8 | +0.00 | - | +99.81 | 525.8 | -1.0 | - |
| burst:5,8 | 2000 ms | own-old | -15.0 | -12.2 | +0.04 | 0.8 | +99.96 | 2633.0 | -1.0 | - |
| burst:5,8 | 2000 ms | own-new | -62.1 | -35.3 | -0.01 | -1.0 | +100.00 | - | -1.0 | - |
| random:2 | 200 ms | own-old | +5.7 | 5.3 | -0.01 | -0.3 | +43.58 | 17.1 | -38.0 | -7.5 |
| random:2 | 200 ms | own-new | +4.3 | 5.0 | +0.02 | 0.4 | +43.13 | 22.3 | -38.0 | -7.5 |
| burst:5,8 | 200 ms | own-old | -0.4 | -0.4 | -0.07 | -0.2 | +43.32 | 9.2 | -75.7 | -4.8 |
| burst:5,8 | 200 ms | own-new | -1.0 | -0.5 | -0.01 | -0.0 | +45.37 | 11.1 | -75.7 | -4.8 |

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 91.1%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 200 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 73.6%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 89.3%
  own-new at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 18.1%
