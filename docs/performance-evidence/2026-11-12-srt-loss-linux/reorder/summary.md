Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Encryption: aes256, no key refresh

WARNING: the proxy reordered packets in 20 trials; those numbers are suspect


Unqualified or incomplete trials: 1 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own | 3 | 66.0% ± 0.6 | 0.00% ± 0.00 | 99.81% ± 0.33 | 99.64% ± 0.62 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 69.4% ± 0.3 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:5 | 2000 ms | own | 3 | 67.2% ± 0.6 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:5 | 2000 ms | libsrt | 3 | 71.3% ± 0.6 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own | 3 | 65.3% ± 1.6 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 70.3% ± 0.6 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own | -3.4 | -8.2 | +0.00 | - | +99.81 | 525.8 | -1.0 | - |
| random:5 | 2000 ms | own | -4.2 | -8.6 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own | -5.0 | -5.1 | +0.00 | - | +100.00 | - | -1.0 | - |

clean runs (the baseline each receiver is compared with):
  own at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 66.1%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
