Encryption: aes256, no key refresh

WARNING: the proxy reordered packets in 18 trials; those numbers are suspect

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| burst:5,8 | 200 ms | own-old | 8 | 70.4% ± 1.3 | 0.12% ± 0.12 | 96.04% ± 1.36 | 99.57% ± 0.51 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 8 | 70.7% ± 1.1 | 0.16% ± 0.11 | 95.19% ± 2.43 | 99.79% ± 0.43 | 0.0 ± 0.0 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 65.4%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 65.1%
