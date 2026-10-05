# Where the iPod app builds (build.sh, started by tools/ipod.sh): swift.org's Swift and clang, lld's Mach-O
# linker, Rust for aarch64-apple-ios with sccache, and Procursus' ldid. build.sh fetches the iPhoneOS SDK and
# Swift's iOS parts into the host's cache.
ARG SWIFT=6.3.3
FROM swift:${SWIFT}-noble
ARG SWIFT
ENV SWIFT_VERSION=${SWIFT}

RUN apt-get update \
 && apt-get install -y --no-install-recommends lld build-essential curl ca-certificates python3 zip rsync \
      xz-utils libarchive-tools \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal \
      --default-toolchain stable --target aarch64-apple-ios \
 && chmod -R a+rX /opt/rustup /opt/cargo
# cc-rs on a non-Apple host: clang for the iOS C/C++, llvm-ar to archive it.
ENV CC_aarch64_apple_ios=clang CXX_aarch64_apple_ios=clang++ AR_aarch64_apple_ios=llvm-ar

# .cargo/config.toml compiles through sccache; its cache lands in the host's (HOME is there).
RUN curl -sSfL "https://github.com/mozilla/sccache/releases/download/v0.18.0/sccache-v0.18.0-$(uname -m)-unknown-linux-musl.tar.gz" \
  | tar -xzf - -C /usr/local/bin --strip-components 1 --wildcards '*/sccache'

RUN curl -sSfL -o /usr/local/bin/ldid \
      "https://github.com/ProcursusTeam/ldid/releases/download/v2.1.5-procursus7/ldid_linux_$(uname -m)" \
 && chmod +x /usr/local/bin/ldid
