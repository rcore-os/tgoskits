FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
ARG QEMU_VERSION=10.2.1

ENV TZ=Etc/UTC \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/qemu-${QEMU_VERSION}/bin:/opt/x86_64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:/opt/riscv64-linux-musl-cross/bin:/opt/loongarch64-linux-musl-cross/bin:${PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bzip2 \
        ca-certificates \
        build-essential \
        clang \
        cmake \
        curl \
        dosfstools \
        e2fsprogs \
        git \
        libglib2.0-dev \
        libclang-19-dev \
        libpixman-1-dev \
        libslirp-dev \
        libudev-dev \
        make \
        meson \
        ninja-build \
        pkg-config \
        python3 \
        python3-venv \
        qemu-user-static \
        wget \
        xz-utils \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    wget -O /tmp/qemu.tar.xz "https://download.qemu.org/qemu-${QEMU_VERSION}.tar.xz"; \
    tar -xf /tmp/qemu.tar.xz -C /tmp; \
    cd "/tmp/qemu-${QEMU_VERSION}"; \
    ./configure \
      --prefix="/opt/qemu-${QEMU_VERSION}" \
      --target-list=loongarch64-softmmu,loongarch64-linux-user,riscv64-softmmu,riscv64-linux-user,aarch64-softmmu,aarch64-linux-user,x86_64-softmmu,x86_64-linux-user \
      --enable-slirp \
      --disable-docs \
      --disable-gtk \
      --disable-sdl \
      --disable-vte \
      --disable-werror; \
    make -j"$(nproc)"; \
    make install; \
    rm -rf "/tmp/qemu-${QEMU_VERSION}" /tmp/qemu.tar.xz

RUN set -eux; \
    for arch in aarch64 riscv64 x86_64 loongarch64; do \
      file="${arch}-linux-musl-cross.tgz"; \
      if ! wget -q "https://github.com/arceos-org/setup-musl/releases/download/prebuilt/${file}" -O "/tmp/${file}"; then \
        if [ "${arch}" = "loongarch64" ]; then \
          wget -q "https://github.com/LoongsonLab/oscomp-toolchains-for-oskernel/releases/download/loongarch64-linux-musl-cross-gcc-13.2.0/${file}" -O "/tmp/${file}"; \
        else \
          wget -q "https://musl.cc/${file}" -O "/tmp/${file}"; \
        fi; \
      fi; \
      tar -xzf "/tmp/${file}" -C /opt; \
      rm -f "/tmp/${file}"; \
    done

COPY rust-toolchain.toml /tmp/rust-toolchain/rust-toolchain.toml

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain none

RUN set -eux; \
    cd /tmp/rust-toolchain; \
    rustc --version; \
    cargo --version; \
    cargo install cargo-binutils axconfig-gen cargo-axplat

WORKDIR /workspace

CMD ["/bin/bash"]
