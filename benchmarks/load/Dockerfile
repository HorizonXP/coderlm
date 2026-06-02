FROM rust:1.87-bookworm AS server-builder
WORKDIR /src/server
COPY server/Cargo.toml server/Cargo.lock ./
COPY server/src ./src
RUN cargo build --release --locked

FROM python:3.12-slim AS harness-base
WORKDIR /workspace
ENV PYTHONUNBUFFERED=1
COPY benchmarks ./benchmarks

FROM harness-base AS server-runtime
COPY --from=server-builder /src/server/target/release/coderlm-server /usr/local/bin/coderlm-server
ENTRYPOINT ["python", "-m", "benchmarks.load.compose_server"]

FROM harness-base AS load-runner
ENTRYPOINT ["python", "-m", "benchmarks.load.runner"]
