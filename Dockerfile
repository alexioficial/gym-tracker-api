# syntax=docker/dockerfile:1.7
# Coolify builds this image from the gym-tracker-api repository/directory.
FROM rust:1.88-bookworm AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Keep downloaded crates and compiled dependencies between BuildKit builds.
# Source changes still force this step to run, but Cargo recompiles only this
# application instead of the complete dependency graph. Touching both entry
# points also guarantees that a cached target can never leave a stale binary.
# The finished executables are copied out because cache-mount contents are not
# part of the resulting image layer.
RUN --mount=type=cache,id=gym-tracker-api-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
	--mount=type=cache,id=gym-tracker-api-cargo-git,target=/usr/local/cargo/git,sharing=locked \
	--mount=type=cache,id=gym-tracker-api-target,target=/app/target,sharing=locked \
	touch src/main.rs src/bin/clone-user-data.rs \
	&& cargo build --locked --release --bins \
	&& install -Dm755 target/release/gym-tracker-api /out/gym-tracker-api \
	&& install -Dm755 target/release/clone-user-data /out/clone-user-data

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl \
	&& rm -rf /var/lib/apt/lists/* \
	&& groupadd --system app \
	&& useradd --system --gid app --create-home --home-dir /app app
WORKDIR /app
COPY --from=build --chown=app:app /out/gym-tracker-api /usr/local/bin/gym-tracker-api
COPY --from=build --chown=app:app /out/clone-user-data /usr/local/bin/gym-tracker-clone-user

# Coolify can override HOST/PORT. RUST_ENV enables the API's strict production
# configuration checks (HTTPS frontend origin and explicit secrets).
# The healthcheck intentionally lives in Coolify's UI, not here: that lets its
# startup grace period be tuned for database/index initialization.
ENV RUST_ENV=production
ENV HOST=0.0.0.0
ENV PORT=8080
# Coolify/Cloudflare provide the original client IP through proxy headers.
# Keep the origin firewalled so clients cannot inject these headers directly.
ENV TRUST_PROXY_HEADERS=true
EXPOSE 8080
USER app
CMD ["/usr/local/bin/gym-tracker-api"]
