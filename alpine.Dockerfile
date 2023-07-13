# fetch the vendor with the builder platform to avoid qemu issues
FROM --platform=$BUILDPLATFORM rust:1-alpine AS vendor

ENV USER=root

WORKDIR /code
RUN cargo init
COPY Cargo.toml /code/Cargo.toml
COPY Cargo.lock /code/Cargo.lock

# https://docs.docker.com/engine/reference/builder/#run---mounttypecache
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

FROM scratch AS binary

COPY --from=builder /code/target/release/catapulte /catapulte

FROM alpine:3

LABEL org.label-schema.schema-version="1.0"
LABEL org.label-schema.docker.cmd="docker run -d -p 3000:3000 -e TEMPLATE__TYPE=LOCAL -e TEMPLATE__PATH=/templates -e SMTP__HOSTNAME=localhost -e SMTP__PORT=25 -e SMTP__USERNAME=username -e SMTP__PASSWORD=password -e SMTP__MAX_POOL_SIZE=10 jdrouet/catapulte"
LABEL org.label-schema.vcs-url="https://jolimail.io"
LABEL org.label-schema.url="https://github.com/jdrouet/catapulte"
LABEL org.label-schema.description="Service to convert mrml to html and send it by email"
LABEL maintaner="Jeremie Drouet <jeremie.drouet@gmail.com>"

RUN apk add --no-cache curl

ENV HOST=0.0.0.0
ENV PORT=3000
ENV RUST_LOG=info
ENV TEMPLATE_ROOT=/templates
ENV DISABLE_LOG_COLOR=true

COPY --from=builder /code/target/release/catapulte /usr/bin/catapulte

EXPOSE 3000

HEALTHCHECK --interval=10s --timeout=3s \
  CMD curl --fail --head http://localhost:3000/status || exit 1

ENTRYPOINT [ "/usr/bin/catapulte" ]
CMD [ "serve" ]
