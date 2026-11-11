# sapflow-rtp

sapflow's own RTP, RTCP, SRTP and SRTCP, for receive-only WHIP ingest
(`docs/whip-ingest.md` at the repository root). MIT, like the rest of
sapflow.

Written from the RFCs only: RFC 3550 and RFC 3551 (RTP and RTCP), RFC 4585
and RFC 5104 (feedback), RFC 5506 (reduced-size RTCP), RFC 8285 (header
extensions), RFC 3711 (SRTP), RFC 7714 (AES-GCM) and RFC 5764 section 4.2
(DTLS-SRTP keys). No source of another RTP, RTCP or SRTP implementation was
read. FFmpeg is used only as a black box, to capture test traffic.

Work in progress: RTP and RTCP packets, SRTP and SRTCP so far.
