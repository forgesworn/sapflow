Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Unqualified trial: libsrt none 200 ms seed 0 (FAIL)
Encryption: aes256, no key refresh


Unqualified or incomplete trials: 2 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own-old | 3 | 2.0% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | own-new | 3 | 2.0% ± 0.1 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 2.8% ± 0.0 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-old | 3 | 5.3% ± 1.2 | 0.00% ± 0.00 | 100.00% ± 0.00 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | own-new | 3 | 5.4% ± 1.3 | 0.00% ± 0.00 | 100.00% ± 0.00 | 99.81% ± 0.33 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 7.5% ± 1.6 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:2 | 200 ms | own-old | 3 | 3.4% ± 0.2 | 0.01% ± 0.01 | 99.09% ± 1.12 | 99.64% ± 0.62 | 0.0 ± 0.0 |
| random:2 | 200 ms | own-new | 3 | 3.4% ± 0.2 | 0.00% ± 0.00 | 99.51% ± 0.85 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 200 ms | libsrt | 3 | 2.9% ± 0.0 | 0.03% ± 0.01 | 0.00% ± 0.00 | 0.00% ± 0.00 | 2.0 ± 1.0 |
| burst:5,8 | 200 ms | own-old | 3 | 8.6% ± 2.0 | 0.20% ± 0.03 | 94.61% ± 1.49 | 99.17% ± 0.99 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 3 | 9.1% ± 1.8 | 0.27% ± 0.23 | 94.46% ± 2.55 | 99.26% ± 0.29 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | libsrt | 3 | 7.4% ± 1.8 | 0.24% ± 0.17 | 0.00% ± 0.00 | 0.00% ± 0.00 | 4.3 ± 3.1 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own-old | -0.8 | -15.3 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 2000 ms | own-new | -0.8 | -18.9 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-old | -2.2 | -1.9 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own-new | -2.1 | -1.8 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:2 | 200 ms | own-old | +0.6 | 4.0 | -0.02 | -2.1 | +99.09 | 153.0 | -2.0 | -3.5 |
| random:2 | 200 ms | own-new | +0.5 | 4.4 | -0.03 | -4.0 | +99.51 | 201.6 | -2.0 | -3.5 |
| burst:5,8 | 200 ms | own-old | +1.3 | 0.8 | -0.04 | -0.4 | +94.61 | 110.2 | -4.3 | -2.5 |
| burst:5,8 | 200 ms | own-new | +1.7 | 1.2 | +0.02 | 0.1 | +94.46 | 64.2 | -4.3 | -2.5 |

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 200 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-new at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
