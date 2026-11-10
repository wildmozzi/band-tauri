# band-relay: UDP 릴레이 (fly.io). 빌드 컨텍스트는 relay/ 디렉터리.
FROM rust:1-slim-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd --system --no-create-home relay
COPY --from=builder /src/target/release/band-relay /usr/local/bin/band-relay
USER relay
# fly.io UDP는 fly-global-services 주소에 바인드해야 한다 (fly.toml [env]에서 지정)
ENV RELAY_BIND=0.0.0.0:5000 \
    RELAY_METRICS_BIND=[::]:9090
EXPOSE 5000/udp 9090/tcp
CMD ["band-relay"]
