# syntax=docker/dockerfile:1.7
# Coolify builds this image from the gym-tracker-api repository/directory.
FROM rust:1.88-bookworm AS build
WORKDIR /app

# Compile the real source in the same layer as it is copied.  A previous
# placeholder-main cache strategy could leave Cargo believing the placeholder
# binary was newer than source files whose timestamps were preserved by COPY.
# That produced an image which exited successfully without starting the API.
COPY Cargo.toml Cargo.lock ./
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
CMD ["/usr/local/bin/gym-tracker-api"]
