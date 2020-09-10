FROM rust:1-slim-buster AS base

RUN apt-get update \
  && apt-get install -y libssl-dev pkg-config \
  && rm -rf /var/lib/apt/lists/*

ENV USER=root

WORKDIR /code
RUN cargo init
COPY Cargo.toml /code/Cargo.toml
RUN cargo fetch

COPY src /code/src
COPY template /code/template

CMD [ "cargo", "test", "--offline" ]

FROM base AS builder

RUN cargo build --release --offline

FROM debian:buster-slim

RUN apt-get update \
  && apt-get install -y ca-certificates libssl1.1 \
  && rm -rf /var/lib/apt/lists/*

ENV ADDRESS=0.0.0.0
ENV PORT=3000
ENV RUST_LOG=info
ENV TEMPLATE_ROOT=/templates

COPY --from=builder /code/target/release/catapulte /usr/bin/catapulte

EXPOSE 3000

ENTRYPOINT [ "/usr/bin/catapulte" ]
