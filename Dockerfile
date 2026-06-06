FROM lukemathwalker/cargo-chef:latest-rust-1.95.0-alpine3.23 AS chef
WORKDIR /app

FROM chef AS planner

COPY . .

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --bin catapulte --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin catapulte
# Stage a data directory owned by the runtime user. A named volume mounted here
# inherits this ownership on first creation, so the container needs no root.
RUN mkdir -p /data

FROM 11notes/distroless:latest AS runtime

COPY --from=builder /app/target/release/catapulte /usr/local/bin/catapulte
COPY --from=builder --chown=65532:65532 /data /data

ENV CATAPULTE_HTTP_ADDRESS=0.0.0.0:3000
ENV CATAPULTE_SQLITE_URL=:memory:

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 CMD ["/usr/local/bin/catapulte", "healthcheck"]

USER 65532:65532
ENTRYPOINT ["/usr/local/bin/catapulte"]
