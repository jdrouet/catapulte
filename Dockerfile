FROM rust:1-slim-buster AS base

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

FROM rust:1-slim-buster

ENV ADDRESS=0.0.0.0
ENV PORT=3000
ENV RUST_LOG=info

COPY --from=builder /code/target/release/catapulte /usr/bin/catapulte

EXPOSE 3000

ENTRYPOINT [ "/usr/bin/catapulte" ]
