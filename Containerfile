FROM docker.io/rust:alpine as base
WORKDIR /whf
RUN apk add musl-dev
ENV \
  CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
  CARGO_TARGET_DIR=/target
RUN cargo install cargo-chef@0.1.71 --locked -q

FROM base as planner
COPY . .
RUN cargo chef prepare

FROM base as build
# Use cargo chef to cook dependencies and cache
COPY --from=planner /whf/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --locked -p webhook-forwarder-server

# Actually build things
COPY . .
# Build the server
RUN cargo build --release --locked -p webhook-forwarder-server

# Create production image
FROM scratch as server
WORKDIR /whf
COPY --from=build /target/release/webhook-forwarder-server /bin/whf

ENV \
  RUST_LOG=info \
  PORT=80
EXPOSE 80

ENTRYPOINT ["whf"]
