Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Unqualified trial: libsrt none 200 ms seed 0 (FAIL)
Encryption: aes256, no key refresh


Unqualified or incomplete trials: 2 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own-old | 3 | 2.0% ± 0.0 | 0.00% ± 0.01 | 100.00% ± 0.00 | 99.86% ± 0.25 | 0.0 ± 0.0 |
| random:2 | 2000 ms | own-new | 3 | 2.0% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 2.0% ± 0.0 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-old | 3 | 5.4% ± 1.3 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-new | 3 | 5.5% ± 1.4 | 0.00% ± 0.00 | 100.80% ± 1.38 | 100.19% ± 0.33 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 5.8% ± 1.1 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:2 | 200 ms | own-old | 3 | 2.5% ± 0.2 | 0.00% ± 0.00 | 100.80% ± 1.38 | 100.19% ± 0.33 | 0.0 ± 0.0 |
| random:2 | 200 ms | own-new | 3 | 2.6% ± 0.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| random:2 | 200 ms | libsrt | 3 | 2.1% ± 0.0 | 0.00% ± 0.01 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 200 ms | own-old | 3 | 7.4% ± 1.3 | 0.12% ± 0.21 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 3 | 6.4% ± 1.8 | 0.19% ± 0.10 | 98.06% ± 1.48 | 99.62% ± 0.66 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | libsrt | 3 | 5.8% ± 1.3 | 0.04% ± 0.02 | 0.00% ± 0.00 | 0.00% ± 0.00 | 2.7 ± 1.5 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own-old | -0.0 | -1.4 | +0.00 | 1.0 | +100.00 | - | -1.0 | - |
| random:2 | 2000 ms | own-new | -0.0 | -0.2 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-old | -0.3 | -0.4 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-new | -0.3 | -0.3 | +0.00 | - | +100.80 | 126.4 | -1.0 | - |
| random:2 | 200 ms | own-old | +0.4 | 4.5 | -0.00 | -1.0 | +100.80 | 126.4 | -1.0 | - |
| random:2 | 200 ms | own-new | +0.5 | 15.3 | -0.00 | -1.0 | +100.00 | - | -1.0 | - |
| burst:5,8 | 200 ms | own-old | +1.7 | 1.6 | +0.08 | 0.6 | +100.00 | - | -2.7 | -3.0 |
| burst:5,8 | 200 ms | own-new | +0.7 | 0.5 | +0.14 | 2.3 | +98.06 | 114.7 | -2.7 | -3.0 |

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 200 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-new at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
