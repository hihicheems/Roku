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

# Build with the repository root as context:
#   docker build -f deploy/Dockerfile .
FROM --platform=$TARGETPLATFORM rust:1.95-alpine AS chef
RUN apk add --no-cache build-base musl-dev && \
    cargo install --locked cargo-chef --version 0.1.77
WORKDIR /app

# ────────────────────────────────
# Planner stage
# ────────────────────────────────
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ────────────────────────────────
# Builder stage
# ────────────────────────────────
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --locked
COPY . .
RUN cargo build --release --locked --package roku-cmd

# ────────────────────────────────
# Runtime stage
# ────────────────────────────────
FROM --platform=$TARGETPLATFORM alpine:3.24 AS runtime

# Install CA certificates for HTTPS
RUN apk add --no-cache ca-certificates

# Create non-root user
RUN addgroup -S -g 1000 appuser && \
    adduser -S -D -H -u 1000 -G appuser appuser

WORKDIR /app
ENV HOME=/app
ENV ROKU_HOME=/app/.roku
ENV ROKU_API_BIND_ADDR=0.0.0.0:8787

COPY --from=builder /app/target/release/roku-cmd /app/roku-cmd
COPY --from=builder /app/config /app/config

# Set ownership
RUN mkdir -p /app/.roku && chown -R appuser:appuser /app
# Switch to non-root user
USER appuser

EXPOSE 8787

ENTRYPOINT ["/app/roku-cmd"]
CMD ["--help"]
