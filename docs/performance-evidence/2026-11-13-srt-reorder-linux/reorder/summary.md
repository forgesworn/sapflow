Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Unqualified trial: libsrt none 200 ms seed 0 (FAIL)
Encryption: aes256, no key refresh

WARNING: the proxy reordered packets in 42 trials; those numbers are suspect


Unqualified or incomplete trials: 2 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own-old | 3 | 66.5% ± 0.7 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | own-new | 3 | 3.1% ± 0.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 69.4% ± 0.1 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-old | 3 | 65.1% ± 1.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-new | 3 | 6.4% ± 1.3 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 69.7% ± 0.6 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:2 | 200 ms | own-old | 3 | 67.7% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 200 ms | own-new | 3 | 67.4% ± 0.4 | 0.00% ± 0.00 | 100.00% ± 0.00 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| random:2 | 200 ms | libsrt | 3 | 69.2% ± 0.2 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 200 ms | own-old | 3 | 68.9% ± 0.7 | 0.07% ± 0.05 | 95.25% ± 1.73 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 3 | 68.6% ± 1.2 | 0.16% ± 0.14 | 96.09% ± 4.93 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | libsrt | 3 | 69.9% ± 0.7 | 0.17% ± 0.09 | 0.00% ± 0.00 | 0.00% ± 0.00 | 4.0 ± 2.0 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own-old | -3.0 | -7.0 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 2000 ms | own-new | -66.3 | -807.5 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-old | -4.6 | -6.7 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-new | -63.4 | -75.4 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 200 ms | own-old | -1.5 | -12.2 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 200 ms | own-new | -1.8 | -6.4 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 200 ms | own-old | -1.0 | -1.7 | -0.10 | -1.8 | +95.25 | 95.4 | -4.0 | -3.5 |
| burst:5,8 | 200 ms | own-new | -1.3 | -1.6 | -0.01 | -0.1 | +96.09 | 33.7 | -4.0 | -3.5 |

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 65.7%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 200 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 65.5%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 64.7%
  own-new at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 1.0%
