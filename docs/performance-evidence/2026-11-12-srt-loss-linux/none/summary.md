Unqualified trial: libsrt none 2000 ms seed 0 (FAIL)
Encryption: aes256, no key refresh


Unqualified or incomplete trials: 1 (retained in results.tsv)

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| random:2 | 2000 ms | own | 3 | 2.0% ± 0.0 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:2 | 2000 ms | libsrt | 3 | 2.2% ± 0.1 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| random:5 | 2000 ms | own | 3 | 5.4% ± 0.2 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| random:5 | 2000 ms | libsrt | 3 | 5.8% ± 0.3 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |
| burst:5,8 | 2000 ms | own | 3 | 5.5% ± 1.3 | 0.00% ± 0.00 | 100.00% ± 0.00 | 100.00% ± 0.00 | 0.0 ± 0.0 |
| burst:5,8 | 2000 ms | libsrt | 3 | 6.2% ± 1.6 | 0.00% ± 0.00 | 0.00% ± 0.00 | 0.00% ± 0.00 | 1.0 ± 0.0 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|
| random:2 | 2000 ms | own | -0.1 | -2.7 | +0.00 | - | +100.00 | - | -1.0 | - |
| random:5 | 2000 ms | own | -0.4 | -2.0 | +0.00 | - | +100.00 | - | -1.0 | - |
| burst:5,8 | 2000 ms | own | -0.6 | -0.5 | +0.00 | - | +100.00 | - | -1.0 | - |

clean runs (the baseline each receiver is compared with):
  own at 2000 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  libsrt at 2000 ms: 0 video, 0 audio packets, 0 decode messages, retransmission overhead 0.0%
