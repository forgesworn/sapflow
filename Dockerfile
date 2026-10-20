# sapflow container image: build with the pinned toolchain, run as non-root
# on a minimal base with only the binary and its licences.
#
#   docker build -t sapflow .
#   docker run --rm -p 8890:8890/udp -p 1935:1935 -p 8888:8888 \
#     -e SAPFLOW_PUBLISH_USER=... -e SAPFLOW_PUBLISH_PASS=... sapflow

FROM rust:1.94.1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p sapflow-server \
 && strip target/release/sapflow

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/sapflow /usr/local/bin/sapflow
COPY LICENSE THIRD_PARTY_LICENSES.txt /usr/share/doc/sapflow/
# SRT (UDP), RTMP, HLS.
EXPOSE 8890/udp 1935 8888
# Recordings, if SAPFLOW_RECORDINGS=/recordings; mount a volume there.
VOLUME ["/recordings"]
USER nonroot
ENTRYPOINT ["/usr/local/bin/sapflow"]
