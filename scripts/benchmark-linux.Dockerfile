FROM public.ecr.aws/docker/library/rust:1.97-bookworm AS builder

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        libwebkit2gtk-4.1-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /source
COPY . .
RUN --mount=type=cache,id=keencode-bench-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=keencode-bench-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=keencode-bench-target,target=/source/src-tauri/target \
    cargo build --manifest-path src-tauri/Cargo.toml -p keencode-desktop \
        --features benchmark --example keencode-bench --release \
    && mkdir -p /out \
    && cp /source/src-tauri/target/release/examples/keencode-bench /out/keencode-bench

FROM scratch AS export
COPY --from=builder /out/keencode-bench /keencode-bench
