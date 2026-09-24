# Cross-compiling multi-arch build with a durable cargo-chef dependency layer.
#
# The builder runs on the NATIVE build platform (`--platform=$BUILDPLATFORM`)
# and cross-compiles to the requested target arch. Previously the builder ran
# under QEMU for linux/arm64, which emulated the *entire* Rust compile and took
# ~2h per release. Cross-compiling on the native runner brings arm64 back to
# minutes; only the tiny runtime layer (ca-certificates) still touches QEMU.
#
# cargo-chef splits the expensive external-crate compile (aws-lc-sys, chromium/
# CDP, tokio, rustls, ...) into its own `cacher` stage keyed ONLY on the Cargo
# manifests (recipe.json). A source-only change leaves recipe.json byte-
# identical -> the cook is reused and only the workspace crates recompile.
#
# DURABILITY: prod (rolling-deploy.sh) bakes the `cacher` stage as a TAGGED
# image `crw-api-deps:<fp>` and overrides DEPS_STAGE to it, so `builder` runs
# `FROM crw-api-deps:<fp>`. Tagged image layers are referenced (not dangling)
# and are NOT build cache, so the nightly `docker image prune -f` +
# `docker builder prune --keep-storage 1GB` cannot evict them. CI leaves
# DEPS_STAGE=cacher (cooks inline; gha mode=max caches the cacher layer).

# Global ARG (before the first FROM) so it is usable in the `builder` FROM line.
ARG DEPS_STAGE=cacher

# Which workspace binaries to cook + build. Default = all three (the public
# open-core image ships crw + crw-server + crw-mcp). Prod overrides the ARG with
# its own package/feature set: the engine container only runs crw-server
# (CMD), so building crw-cli + crw-mcp there is wasted work — dropping them
# removes two of three thin-LTO links (faster + far less CPU contention with the
# live engine). MUST be identical in the cook and the build (fingerprint match).
ARG CARGO_PKGS="-p crw-server --features cdp,impersonated -p crw-mcp -p crw-cli"

# Cap on compile parallelism. cargo reads CARGO_BUILD_JOBS natively (BuildKit
# injects a declared ARG into the RUN env), so no -j flag is needed. "default" =
# all cores (CI); prod passes a number below the core count so a build never
# starves the live engine of CPU. Must NOT be empty — cargo rejects an empty
# value ("could not parse ``").
ARG CARGO_BUILD_JOBS=default

# ---- shared toolchain base --------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:1.98-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS chef

# Provided automatically by buildx: amd64 | arm64.
ARG TARGETARCH
WORKDIR /app

# Native build deps + Rust target + (arm64) cross linker toolchain; record the
# target triple.
#
# cmake + libclang are for btls-sys, the vendored BoringSSL behind wreq (the
# `impersonated` feature this image builds with). It compiles BoringSSL through
# CMake and generates its bindings with bindgen, which dlopens libclang at build
# time. rust:bookworm derives from buildpack-deps, which ships gcc/g++/make and
# autotools but NEITHER cmake NOR clang, so without this line the cook fails with
# "is `cmake` not installed?". Both arches need it: the aarch64 leg cross-compiles
# BoringSSL and cross-generates the bindings from the same host clang.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends cmake clang libclang-dev; \
    case "$TARGETARCH" in \
      amd64) RUST_TARGET=x86_64-unknown-linux-gnu ;; \
      arm64) RUST_TARGET=aarch64-unknown-linux-gnu; \
             # crossbuild-essential-arm64 = the aarch64 gcc/g++ AND the target
             # libc dev headers (libc6-dev-arm64-cross). The bare cross gcc
             # alone lacks sys/types.h etc., which broke aws-lc-sys's C build,
             # and btls-sys's cross build needs the same headers.
             apt-get install -y --no-install-recommends crossbuild-essential-arm64 ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$RUST_TARGET"; \
    echo "$RUST_TARGET" > /rust_target

# cargo-chef as a host (BUILDPLATFORM) tool. Pinned so the install layer is
# deterministic; must be a release that parses this edition-2024 / resolver-2
# workspace (verify `cargo chef prepare` succeeds before merge — see PR checklist).
RUN cargo install cargo-chef --locked --version 0.1.77

# Shared build env — MUST be identical in `cacher` (cook) and `builder` (final
# compile) or the cooked deps get a different cargo fingerprint and recompile.
# Baked into `chef` so both inherit it verbatim (and so the tagged deps image
# carries it too).
#   - Linker for the aarch64 cross target (ignored on native amd64).
#   - Workspace release profile is fat LTO + codegen-units=1, whose final link
#     of crw-server (aws-lc-sys + full graph) needs several GB and OOM-killed
#     the build (#90). thin LTO + 16 CGUs: far lower peak memory, faster link,
#     negligible runtime difference.
#   - C/C++ cross compilers for the aarch64 target. btls-sys (the vendored
#     BoringSSL behind wreq) applies its own CMAKE_TOOLCHAIN_FILE for aarch64,
#     and that file sets only CMAKE_SYSTEM_NAME/PROCESSOR: "Rely on environment
#     variables to set the compiler and include paths." Because a toolchain file
#     is present, the cmake crate does not inject CMAKE_C_COMPILER either, so
#     CMake fell back to the HOST cc and produced x86-64 objects. The aarch64
#     link then died with "Relocations in generic ELF (EM: 62)" on
#     libbtls_sys.rlib. These two are what the cc crate reads for the target,
#     and they reach CMake through it. They are per-target names, so the amd64
#     leg ignores them.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

# ---- planner: derive the dependency recipe from the manifests only ----------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path /recipe.json

# ---- cacher: compile ONLY the dependency graph (the durable, taggable layer) -
# Keyed solely on /recipe.json; SAME --target/--features/-p set (CARGO_PKGS) as
# the real build so the cooked artifacts fingerprint-match and are reused.
FROM chef AS cacher
ARG CARGO_PKGS
ARG CARGO_BUILD_JOBS
COPY --from=planner /recipe.json /recipe.json
# $CARGO_PKGS is intentionally unquoted so it word-splits into cargo args.
# Parallelism comes from the CARGO_BUILD_JOBS env (injected from the ARG).
RUN set -eux; \
    RUST_TARGET="$(cat /rust_target)"; \
    cargo chef cook --release --target "$RUST_TARGET" \
      $CARGO_PKGS \
      --recipe-path /recipe.json

# ---- builder: compile the workspace crates on top of the cooked deps --------
# DEPS_STAGE defaults to the in-tree `cacher` (CI / any plain `docker build`
# cooks deps inline). Prod overrides it to the pinned crw-api-deps:<fp> image.
FROM ${DEPS_STAGE} AS builder
ARG CARGO_PKGS
ARG CARGO_BUILD_JOBS
COPY . .
# Build the CARGO_PKGS set, then copy whatever binaries were produced (the set
# is variable: all three by default, only crw-server in prod).
RUN set -eux; \
    RUST_TARGET="$(cat /rust_target)"; \
    cargo build --release --target "$RUST_TARGET" \
      $CARGO_PKGS; \
    mkdir -p /out; \
    for b in crw crw-server crw-mcp; do \
      f="target/${RUST_TARGET}/release/$b"; \
      if [ -f "$f" ]; then cp "$f" /out/; fi; \
    done; \
    test -f /out/crw-server   # crw-server is required in every build

# ---- runtime (unchanged) ----------------------------------------------------
FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy whatever binaries the builder produced (all three for the public image;
# just crw-server for the prod engine build). A directory copy tolerates the
# variable set — no per-binary COPY that would fail when a binary was skipped.
COPY --from=builder /out/ /usr/local/bin/
COPY config.default.toml /app/config.default.toml
COPY config.docker.toml /app/config.docker.toml

WORKDIR /app

LABEL io.modelcontextprotocol.server.name="io.github.fastcrw/crw"

EXPOSE 3000

CMD ["crw-server"]
