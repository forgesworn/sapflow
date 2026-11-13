Encryption: aes256, no key refresh

| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |
|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|
| burst:5,8 | 200 ms | own-old | 8 | 7.2% ± 0.8 | 0.11% ± 0.11 | 98.76% ± 1.39 | 99.95% ± 0.15 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-aa | 8 | 7.1% ± 1.2 | 0.14% ± 0.16 | 98.58% ± 1.70 | 99.87% ± 0.23 | 0.0 ± 0.0 |
| burst:5,8 | 200 ms | own-new | 8 | 7.3% ± 1.3 | 0.13% ± 0.14 | 97.96% ± 2.12 | 99.86% ± 0.26 | 0.0 ± 0.0 |

Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise).
Where decode messages differ, packets delivered count different things: libsrt
writes damaged packets through, sapflow drops what it cannot use.

| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |
|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|

clean runs (the baseline each receiver is compared with):
  own-new at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-aa at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
  own-old at 200 ms: 878 video, 1399 audio packets, 0 decode messages, retransmission overhead 0.0%
