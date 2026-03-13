# syntax=docker/dockerfile:1.7

# ── Stage 1: Build Web UI ─────────────────────────────────────
FROM node:22-bookworm-slim AS web_builder

WORKDIR /web

COPY web/package.json web/package-lock.json ./
RUN npm ci

COPY web/ ./
RUN npm run build

# ── Stage 2: Build Rust Binary ────────────────────────────────
FROM rust:1.93-slim@sha256:7e6fa79cf81be23fd45d857f75f583d80cfdbb11c91fa06180fd747fda37a61d AS builder

WORKDIR /app
ARG LLAMAFARM_CARGO_FEATURES=""

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml
# Create dummy targets declared in Cargo.toml so manifest parsing succeeds.
RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs
RUN --mount=type=cache,id=llamafarm-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=llamafarm-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=llamafarm-target,target=/app/target,sharing=locked \
    if [ -n "$LLAMAFARM_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$LLAMAFARM_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi
RUN rm -rf src benches crates/robot-kit/src

# 2. Copy only build-relevant source paths (avoid cache-busting on docs/tests/scripts)
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY firmware/ firmware/
COPY data/ data/
COPY skills/ skills/
RUN mkdir -p web/dist
COPY --from=web_builder /web/dist/ web/dist/
RUN --mount=type=cache,id=llamafarm-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=llamafarm-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=llamafarm-target,target=/app/target,sharing=locked \
    if [ -n "$LLAMAFARM_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$LLAMAFARM_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi && \
    cp target/release/llamafarm /app/llamafarm && \
    strip /app/llamafarm

# Prepare runtime directory structure and default config inline (no extra stage)
RUN mkdir -p /llamafarm-data/.llamafarm /llamafarm-data/workspace && \
    cat > /llamafarm-data/.llamafarm/config.toml <<EOF && \
    chmod 600 /llamafarm-data/.llamafarm/config.toml && \
    chown -R 65534:65534 /llamafarm-data
workspace_dir = "/llamafarm-data/workspace"
config_path = "/llamafarm-data/.llamafarm/config.toml"
api_url = "http://host.docker.internal:11434"
default_provider = "ollama"
default_model = "qwen3.5:9b"
default_temperature = 0.7

[gateway]
port = 42617
host = "0.0.0.0"
require_pairing = false
allow_public_bind = true
EOF

# ── Stage 2: Development Runtime (Debian) ────────────────────
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS dev

# Install the local engineering runtime toolchain used by the God/Safe profiles.
RUN apt-get update && apt-get install -y \
    bash \
    build-essential \
    ca-certificates \
    cmake \
    curl \
    file \
    git \
    iproute2 \
    iputils-ping \
    jq \
    make \
    net-tools \
    nodejs \
    npm \
    pciutils \
    procps \
    pkg-config \
    python-is-python3 \
    python3 \
    python3-pip \
    python3-venv \
    ripgrep \
    rsync \
    sqlite3 \
    util-linux \
    usbutils \
    wget \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /llamafarm-data /llamafarm-data
COPY --from=builder /app/llamafarm /usr/local/bin/llamafarm
COPY dev/config.template.toml /usr/share/llamafarm/config.template.toml
COPY dev/config.preset.safe.toml /usr/share/llamafarm/config.preset.safe.toml
COPY dev/workspace.preset.god.AGENTS.md /usr/share/llamafarm/workspace.preset.god.AGENTS.md
COPY dev/workspace.preset.god.SOUL.md /usr/share/llamafarm/workspace.preset.god.SOUL.md
COPY dev/workspace.preset.safe.AGENTS.md /usr/share/llamafarm/workspace.preset.safe.AGENTS.md
COPY dev/workspace.preset.safe.SOUL.md /usr/share/llamafarm/workspace.preset.safe.SOUL.md
COPY scripts/docker/dev-entrypoint.sh /usr/local/bin/dev-entrypoint.sh
RUN chmod 755 /usr/local/bin/dev-entrypoint.sh && \
    chmod 644 /usr/share/llamafarm/config.template.toml /usr/share/llamafarm/config.preset.safe.toml && \
    rm -f /llamafarm-data/.llamafarm/config.toml

# Environment setup
# Use consistent workspace path
ENV LLAMAFARM_WORKSPACE=/llamafarm-data/workspace
ENV HOME=/llamafarm-data
ENV SHELL=/bin/bash
# Provider/model selection comes from the mounted config so local stacks do not
# silently drift away from the chosen Ollama model.
ENV LLAMAFARM_GATEWAY_PORT=42617

WORKDIR /llamafarm-data
USER 0:0
EXPOSE 42617
ENTRYPOINT ["/usr/local/bin/dev-entrypoint.sh"]
CMD ["llamafarm", "gateway"]

# ── Stage 3: Production Runtime (Distroless) ─────────────────
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:84fcd3c223b144b0cb6edc5ecc75641819842a9679a3a58fd6294bec47532bf7 AS release

COPY --from=builder /app/llamafarm /usr/local/bin/llamafarm
COPY --from=builder /llamafarm-data /llamafarm-data
COPY dev/config.template.toml /usr/share/llamafarm/config.template.toml
COPY dev/config.preset.safe.toml /usr/share/llamafarm/config.preset.safe.toml
COPY dev/workspace.preset.god.AGENTS.md /usr/share/llamafarm/workspace.preset.god.AGENTS.md
COPY dev/workspace.preset.god.SOUL.md /usr/share/llamafarm/workspace.preset.god.SOUL.md
COPY dev/workspace.preset.safe.AGENTS.md /usr/share/llamafarm/workspace.preset.safe.AGENTS.md
COPY dev/workspace.preset.safe.SOUL.md /usr/share/llamafarm/workspace.preset.safe.SOUL.md

# Environment setup
ENV LLAMAFARM_WORKSPACE=/llamafarm-data/workspace
ENV HOME=/llamafarm-data
# Default provider and model are set in config.toml, not here,
# so config file edits are not silently overridden
#ENV PROVIDER=
ENV LLAMAFARM_GATEWAY_PORT=42617

# API_KEY must be provided at runtime!

WORKDIR /llamafarm-data
USER 65534:65534
EXPOSE 42617
ENTRYPOINT ["llamafarm"]
CMD ["gateway"]
