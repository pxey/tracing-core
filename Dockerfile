# مرحلة البناء
FROM rust:1.78-slim AS builder
WORKDIR /build
COPY Cargo.toml ./
COPY src ./src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release

# مرحلة التشغيل — صورة نحيفة تحترم قيود الموارد في القسم 6
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/ctc ./ctc
COPY config ./config
COPY rules ./rules

EXPOSE 9000 9090 9091
ENTRYPOINT ["./ctc", "config/ctc.config.toml"]
