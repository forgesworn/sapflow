# sapflow container image: build with the pinned toolchain, run as non-root
# on a minimal base with only the binary and its licences.
#
#   docker build -t sapflow .                    (profile-guided, slower to build)
#   docker build --build-arg PGO=0 -t sapflow .  (a single plain release build)
#   docker run --rm -p 8890:8890/udp -p 1935:1935 -p 8888:8888 \
#     -e SAPFLOW_PUBLISH_USER=... -e SAPFLOW_PUBLISH_PASS=... sapflow

# The server is built with profile-guided optimisation by default: an
# instrumented build is trained on generated SRT and RTMP traffic on
# loopback inside the build stage, then built again with that profile
# (scripts/build-pgo.sh). It takes three server builds and about a minute of
# training. `--build-arg PGO=0` builds once, without it, for a quick image or
# a builder that cannot run the training.
FROM rust:1.94.1-slim-bookworm AS build
ARG PGO=1
WORKDIR /src
COPY . .
RUN if [ "$PGO" = 1 ]; then \
      apt-get update \
      && apt-get install -y --no-install-recommends ffmpeg srt-tools python3 procps \
      && rm -rf /var/lib/apt/lists/* \
      && rustup component add llvm-tools \
      && bash scripts/build-pgo.sh target/release/sapflow; \
    else \
      cargo build --release --locked -p sapflow; \
    fi \
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
