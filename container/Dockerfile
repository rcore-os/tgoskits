FROM node:24.11.1-bookworm-slim AS node-toolchain

FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
ARG QEMU_VERSION=11.1.1
ARG BUILD_JOBS=4

ENV TZ=Etc/UTC \
    TGOSKITS_PREBUILT_CONTAINER=1 \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/local/bsc/bin:/opt/qemu-${QEMU_VERSION}/bin:/opt/x86_64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:/opt/riscv64-linux-musl-cross/bin:/opt/loongarch64-linux-musl-cross/bin:/usr/sbin:/sbin:${PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        autoconf \
        binutils \
        bison \
        build-essential \
        bzip2 \
        ca-certificates \
        clang \
        cmake \
        coreutils \
        cpio \
        curl \
        device-tree-compiler \
        dosfstools \
        e2fsprogs \
        fakeroot \
        fdisk \
        ffmpeg \
        file \
        flex \
        git \
        git-lfs \
        gperf \
        help2man \
        libavcodec-dev \
        libavdevice-dev \
        libavfilter-dev \
        libavformat-dev \
        libavutil-dev \
        libglib2.0-dev \
        libclang-19-dev \
        libffi-dev \
        libfl-dev \
        libpixman-1-dev \
        libreadline-dev \
        libslirp-dev \
        libswresample-dev \
        libswscale-dev \
        libudev-dev \
        lld \
        make \
        meson \
        mtools \
        ninja-build \
        openjdk-17-jdk-headless \
        perl \
        pkg-config \
        python3 \
        python3-pip \
        python3-venv \
        qemu-user-static \
        ripgrep \
        tcl-dev \
        u-boot-tools \
        unzip \
        wget \
        xz-utils \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

COPY --from=node-toolchain /usr/local/bin/node /usr/local/bin/node
COPY --from=node-toolchain /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

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
    make -j"${BUILD_JOBS}"; \
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
    rg --version; \
    rustc --version; \
    cargo --version; \
    toolchain="$(rustup show active-toolchain)"; \
    rustup default "${toolchain%% *}"; \
    rustup target add \
      x86_64-unknown-linux-musl \
      aarch64-unknown-linux-musl \
      riscv64gc-unknown-linux-musl \
      loongarch64-unknown-linux-musl; \
    cargo install cargo-binutils --locked; \
    rust_sysroot="$(rustc --print sysroot)"; \
    ln -s "${rust_sysroot}"/lib/libLLVM.so.* "${rust_sysroot}/lib/libLLVM.so"; \
    LLVM_PREFIX="${rust_sysroot}" cargo install bpf-linker --version 0.13.0 --locked; \
    rm -rf /opt/cargo/registry /opt/cargo/git

COPY apps/starry/hdl-lang/setup-hdl-toolchain.sh /tmp/setup-hdl-toolchain.sh
RUN INSTALL_SYSTEM_DEPS=0 BSC_ASSET=bsc-2026.01-ubuntu-24.04.tar.gz \
      JOBS="${BUILD_JOBS}" WORK=/tmp/hdl-toolchain \
      bash /tmp/setup-hdl-toolchain.sh \
    && rm -rf /tmp/hdl-toolchain /tmp/setup-hdl-toolchain.sh

COPY container/install-go.sh /tmp/install-go.sh
RUN bash /tmp/install-go.sh /opt/go && rm /tmp/install-go.sh

ENV RUSTUP_AUTO_INSTALL=0 \
    GOROOT=/opt/go \
    GOTOOLCHAIN=local \
    PATH=/opt/go/bin:${PATH}

COPY container/check-env.sh /usr/local/lib/tgoskits/check-env.sh
RUN bash /usr/local/lib/tgoskits/check-env.sh

WORKDIR /workspace

CMD ["/bin/bash"]
