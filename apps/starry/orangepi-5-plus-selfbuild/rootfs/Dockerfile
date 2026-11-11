FROM alpine:3.22

ARG RUST_TOOLCHAIN
ENV HOME=/root CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup
ENV PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apk add --no-cache \
    bash bc binutils build-base ca-certificates clang clang-dev cmake coreutils curl file \
    git jq linux-headers lld llvm-dev openssl-dev eudev-dev \
    make ninja patch pkgconf procps python3 rsync time util-linux xz zlib-dev
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /root/rustup-init.sh \
    && sh /root/rustup-init.sh -y --profile minimal --default-toolchain none \
        --default-host aarch64-unknown-linux-musl \
    && rm /root/rustup-init.sh
RUN rustup toolchain install "$RUST_TOOLCHAIN-aarch64-unknown-linux-musl" --profile minimal \
    --component rust-src --component llvm-tools-preview --component clippy --component rustfmt \
    && rustup default "$RUST_TOOLCHAIN-aarch64-unknown-linux-musl"
COPY registry /root/.cargo/registry
RUN (cargo install --offline --locked --version 0.4.0 cargo-binutils \
        || cargo install --locked --version 0.4.0 cargo-binutils) \
    && (cargo install --offline --locked --version 0.6.0 ksym \
        || cargo install --locked --version 0.6.0 ksym) \
    && rm -rf /root/.cargo/registry \
    && ln -s /usr/bin/gcc /usr/local/bin/aarch64-linux-musl-gcc
RUN mkdir -p /work/targets /output/runs /etc/starry-selfbuild \
    && printf 'rootfs_format=1\nalpine_version=3.22\narchitecture=arm64\nlibc=musl\n' > /.starry-selfbuild-rootfs
COPY rust-toolchain.toml /etc/starry-selfbuild/rust-toolchain.toml
RUN cd /etc/starry-selfbuild && rustup show active-toolchain \
    && rustc --version --verbose \
    && test "$(rustc --print host-tuple)" = aarch64-unknown-linux-musl
COPY cargo-config.toml /root/.cargo/config.toml
COPY registry /root/.cargo/registry
COPY host-link /opt/selfbuild-host-link
RUN cargo build --offline --locked --manifest-path /opt/selfbuild-host-link/Cargo.toml \
        --target aarch64-unknown-none-softfloat --target-dir /host-link-target \
    && echo selfbuild_host_libclang=PASS \
    && rm -rf /host-link-target /opt/selfbuild-host-link /root/.cargo/registry
