# Copyright 2025 itscheems
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

FROM ghcr.io/actions/actions-runner:latest

ARG RUST_TOOLCHAIN_VERSION=1.92.0
ARG GIT_CLIFF_VERSION=v2.12.0
ARG CARGO_NEXTEST_VERSION=cargo-nextest-0.9.132
ARG HAWKEYE_VERSION=v6.5.1
ARG http_proxy
ARG https_proxy
ARG all_proxy
ARG no_proxy
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG ALL_PROXY
ARG NO_PROXY

USER root

RUN set -eux; \
    export http_proxy="${http_proxy:-}" https_proxy="${https_proxy:-}" all_proxy="${all_proxy:-}" no_proxy="${no_proxy:-}" \
      HTTP_PROXY="${HTTP_PROXY:-}" HTTPS_PROXY="${HTTPS_PROXY:-}" ALL_PROXY="${ALL_PROXY:-}" NO_PROXY="${NO_PROXY:-}"; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential \
      pkg-config \
      ca-certificates \
      curl \
      git \
      jq \
      unzip \
      xz-utils; \
    rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/home/runner/.cargo
ENV RUSTUP_HOME=/home/runner/.rustup
ENV ROKU_RUST_BIN=/opt/roku-rust/bin
ENV PATH=${ROKU_RUST_BIN}:/usr/local/bin:${PATH}

RUN install -d -o runner -g runner /home/runner/.cargo /home/runner/.rustup \
    && install -d -m 0755 /opt/roku-rust/bin

USER runner

RUN set -eux; \
    export http_proxy="${http_proxy:-}" https_proxy="${https_proxy:-}" all_proxy="${all_proxy:-}" no_proxy="${no_proxy:-}" \
      HTTP_PROXY="${HTTP_PROXY:-}" HTTPS_PROXY="${HTTPS_PROXY:-}" ALL_PROXY="${ALL_PROXY:-}" NO_PROXY="${NO_PROXY:-}"; \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN_VERSION}" --no-modify-path; \
    rustup component add --toolchain "${RUST_TOOLCHAIN_VERSION}" rustfmt clippy; \
    rustup default "${RUST_TOOLCHAIN_VERSION}"

USER root

RUN set -eux; \
    export http_proxy="${http_proxy:-}" https_proxy="${https_proxy:-}" all_proxy="${all_proxy:-}" no_proxy="${no_proxy:-}" \
      HTTP_PROXY="${HTTP_PROXY:-}" HTTPS_PROXY="${HTTPS_PROXY:-}" ALL_PROXY="${ALL_PROXY:-}" NO_PROXY="${NO_PROXY:-}"; \
    toolchain_bin_dir="$(su runner -s /bin/bash -lc 'dirname "$(rustup which rustc)"')"; \
    rm -f "${ROKU_RUST_BIN}"/*; \
    for tool_path in "${toolchain_bin_dir}"/*; do \
      [ -x "${tool_path}" ] || continue; \
      ln -sf "${tool_path}" "${ROKU_RUST_BIN}/$(basename "${tool_path}")"; \
    done; \
    arch="$(dpkg --print-architecture)"; \
    case "${arch}" in \
      amd64) asset_arch='x86_64-unknown-linux-gnu' ;; \
      arm64) asset_arch='aarch64-unknown-linux-gnu' ;; \
      *) echo "unsupported dpkg architecture: ${arch}" >&2; exit 1 ;; \
    esac; \
    tmpdir="$(mktemp -d)"; \
    curl -fsSL \
      "https://github.com/orhun/git-cliff/releases/download/${GIT_CLIFF_VERSION}/git-cliff-${GIT_CLIFF_VERSION#v}-${asset_arch}.tar.gz" \
      -o "${tmpdir}/git-cliff.tar.gz"; \
    tar -xzf "${tmpdir}/git-cliff.tar.gz" -C "${tmpdir}"; \
    install -m 0755 "$(find "${tmpdir}" -type f -name git-cliff | head -n1)" /usr/local/bin/git-cliff; \
    curl -fsSL \
      "https://github.com/nextest-rs/nextest/releases/download/${CARGO_NEXTEST_VERSION}/${CARGO_NEXTEST_VERSION}-${asset_arch}.tar.gz" \
      -o "${tmpdir}/cargo-nextest.tar.gz"; \
    tar -xzf "${tmpdir}/cargo-nextest.tar.gz" -C "${tmpdir}"; \
    install -m 0755 "$(find "${tmpdir}" -type f -name cargo-nextest | head -n1)" /usr/local/bin/cargo-nextest; \
    curl -fsSL \
      "https://github.com/korandoru/hawkeye/releases/download/${HAWKEYE_VERSION}/hawkeye-${asset_arch}.tar.xz" \
      -o "${tmpdir}/hawkeye.tar.xz"; \
    tar -xJf "${tmpdir}/hawkeye.tar.xz" -C "${tmpdir}"; \
    install -m 0755 "$(find "${tmpdir}" -type f -name hawkeye | head -n1)" /usr/local/bin/hawkeye; \
    rm -rf "${tmpdir}"; \
    chown -R runner:runner /home/runner/.cargo /home/runner/.rustup; \
    test "$(command -v rustc)" = "${ROKU_RUST_BIN}/rustc"; \
    test "$(command -v cargo)" = "${ROKU_RUST_BIN}/cargo"; \
    test "$(command -v rustfmt)" = "${ROKU_RUST_BIN}/rustfmt"; \
    test "$(command -v cargo-fmt)" = "${ROKU_RUST_BIN}/cargo-fmt"; \
    test "$(command -v cargo-clippy)" = "${ROKU_RUST_BIN}/cargo-clippy"; \
    rustc --version; \
    cargo --version; \
    rustfmt --version; \
    cargo-fmt --version; \
    cargo-clippy --version; \
    cc --version; \
    pkg-config --version; \
    git-cliff --version; \
    cargo nextest --version; \
    hawkeye --version

USER runner
