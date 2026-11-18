# band-relay: UDP 릴레이 (fly.io). 빌드 컨텍스트는 relay/ 디렉터리.
# 베이스 이미지는 태그+다이제스트로 고정 (갱신 방법: relay/README.md)
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-20261104-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN useradd --system --no-create-home relay
COPY --from=builder /src/target/release/band-relay /usr/local/bin/band-relay
USER relay
# fly.io UDP는 fly-global-services 주소에 바인드해야 한다 (fly.toml [env]에서 지정)
ENV RELAY_BIND=0.0.0.0:5000 \
    RELAY_METRICS_BIND=[::]:9090
EXPOSE 5000/udp 9090/tcp
CMD ["band-relay"]
