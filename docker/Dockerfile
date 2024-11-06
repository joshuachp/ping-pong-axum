# For all stages
ARG CRATE_NAME=hello-world-axum
ARG CARGO_TARGET_DIR=/build/target

FROM rust:alpine AS builder

ARG CRATE_NAME
ARG CARGO_TARGET_DIR

WORKDIR /build/$CRATE_NAME

RUN apk --no-cache add musl-dev

RUN cargo init

COPY Cargo.toml Cargo.lock /build/$CRATE_NAME

RUN cargo fetch

COPY . /build/$CRATE_NAME

RUN cargo build --release

FROM alpine

ARG CRATE_NAME
ARG CARGO_TARGET_DIR

COPY --from=builder $CARGO_TARGET_DIR/release/$CRATE_NAME /usr/bin

ENTRYPOINT ["entrypoint.sh"]
