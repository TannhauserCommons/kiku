# The Linux build environment for kiku-engine: Ubuntu 22.04 (glibc 2.35, so the result runs on it and
# anything newer), a C++20 compiler and CMake for Moonshine, and Rust. Used by scripts/linux-gate.ps1 and CI.
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake git git-lfs curl ca-certificates pkg-config python3 bzip2 file \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
WORKDIR /src
