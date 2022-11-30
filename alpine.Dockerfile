# fetch the vendor with the builder platform to avoid qemu issues
FROM --platform=$BUILDPLATFORM rust:1-alpine AS vendor

ENV USER=root

WORKDIR /code
RUN cargo init
COPY Cargo.toml /code/Cargo.toml
COPY Cargo.lock /code/Cargo.lock
RUN --mount=type=cache,target=$CARGO_HOME/git,sharing=locked \
  --mount=type=cache,target=$CARGO_HOME/registry,sharing=locked \
  mkdir -p /code/.cargo \
  && cargo vendor > /code/.cargo/config

FROM rust:1-alpine AS base

RUN apk add --no-cache musl-dev

ENV USER=root

WORKDIR /code

COPY Cargo.toml /code/Cargo.toml
COPY Cargo.lock /code/Cargo.lock
COPY src /code/src
COPY --from=vendor /code/.cargo /code/.cargo
COPY --from=vendor /code/vendor /code/vendor

COPY src /code/src
COPY template /code/template

CMD [ "cargo", "test", "--offline" ]

FROM base AS builder

RUN cargo build --release --offline

FROM alpine:3

LABEL org.label-schema.schema-version="1.0"
LABEL org.label-schema.docker.cmd="docker run -d -p 3000:3000 -e TEMPLATE_ROOT=/templates -e SMTP_LOCALHOST=localhost -e SMTP_PORT=25 -e SMTP_USERNAME=username -e SMTP_PASSWORD=password -e SMTP_MAX_POOL_SIZE=10 -e TEMPLATE_PROVIDER=local jdrouet/catapulte"
LABEL org.label-schema.vcs-url="https://jolimail.io"
LABEL org.label-schema.url="https://github.com/jdrouet/catapulte"
LABEL org.label-schema.description="Service to convert mrml to html and send it by email"
LABEL maintaner="Jeremie Drouet <jeremie.drouet@gmail.com>"

RUN apk add --no-cache curl

ENV HOST=0.0.0.0
ENV PORT=3000
ENV RUST_LOG=info
ENV TEMPLATE_ROOT=/templates

COPY --from=builder /code/target/release/catapulte /usr/bin/catapulte

EXPOSE 3000

HEALTHCHECK --interval=10s --timeout=3s \
  CMD curl --fail --head http://localhost:3000/status || exit 1

ENTRYPOINT [ "/usr/bin/catapulte" ]
