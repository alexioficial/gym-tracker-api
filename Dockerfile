# syntax=docker/dockerfile:1.7
# Coolify builds this image from the gym-tracker-api repository/directory.
FROM rust:1.88-bookworm AS build
WORKDIR /app

# Cache dependencies separately from application code.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --locked --release
RUN rm -rf src
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl \
	&& rm -rf /var/lib/apt/lists/* \
	&& groupadd --system app \
	&& useradd --system --gid app --create-home --home-dir /app app
WORKDIR /app
COPY --from=build --chown=app:app /app/target/release/gym-tracker-api /usr/local/bin/gym-tracker-api

# Coolify can override HOST/PORT. RUST_ENV enables the API's strict production
# configuration checks (HTTPS frontend origin and explicit secrets).
# The healthcheck intentionally lives in Coolify's UI, not here: that lets its
# startup grace period be tuned for database/index initialization.
ENV RUST_ENV=production
ENV HOST=0.0.0.0
ENV PORT=8080
EXPOSE 8080
USER app
CMD ["gym-tracker-api"]
