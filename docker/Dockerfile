# Stage 1: Build dynamod from source
FROM ubuntu:24.04 AS builder

ARG ZIG_VERSION=0.15.2
ARG RUST_TARGET=x86_64-unknown-linux-musl

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl ca-certificates xz-utils musl-tools build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
        | tar -xJ -C /opt \
    && ln -s "/opt/zig-x86_64-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal \
    && . "$HOME/.cargo/env" \
    && rustup target add ${RUST_TARGET}

COPY . /src
WORKDIR /src

RUN . "$HOME/.cargo/env" && make all

# Stage 2: Alpine runtime with dynamod as PID 1
FROM alpine:3.21

RUN apk add --no-cache iproute2

COPY --from=builder /src/zig/zig-out/bin/dynamod-init /sbin/dynamod-init

ARG RUST_OUT=/src/rust/target/x86_64-unknown-linux-musl/release
COPY --from=builder ${RUST_OUT}/dynamod-svmgr      /usr/lib/dynamod/dynamod-svmgr
COPY --from=builder ${RUST_OUT}/dynamodctl          /usr/bin/dynamodctl
COPY --from=builder ${RUST_OUT}/dynamod-logd        /usr/lib/dynamod/dynamod-logd
COPY --from=builder ${RUST_OUT}/dynamod-logind      /usr/lib/dynamod/dynamod-logind
COPY --from=builder ${RUST_OUT}/dynamod-sd1bridge   /usr/lib/dynamod/dynamod-sd1bridge
COPY --from=builder ${RUST_OUT}/dynamod-hostnamed   /usr/lib/dynamod/dynamod-hostnamed

COPY config/supervisors/root.toml       /etc/dynamod/supervisors/root.toml
COPY config/supervisors/early-boot.toml /etc/dynamod/supervisors/early-boot.toml
COPY config/supervisors/desktop.toml    /etc/dynamod/supervisors/desktop.toml

COPY docker/services/ /etc/dynamod/services/

RUN mkdir -p /var/lib/dynamod /var/log/dynamod /run/dynamod \
    && echo "dynamod-docker" > /etc/hostname

ENTRYPOINT ["/sbin/dynamod-init"]
