FROM rust:1.70 AS builder
WORKDIR /usr/src/kv_demo

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        libclang-dev

# cache dependencies
RUN cargo init --bin
COPY Cargo.toml ./
# COPY Cargo.lock ./
# RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --release
RUN cargo build --release
RUN rm ./src/*.rs ./target/release/deps/kv_demo*

# build
COPY . .
RUN cargo install --path .

FROM debian:bullseye-slim
COPY --from=builder /usr/local/cargo/bin/kv_demo /usr/local/bin/kv_demo
CMD ["kv_demo"]
