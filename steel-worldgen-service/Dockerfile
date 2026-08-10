FROM rustlang/rust:nightly-2026-07-23@sha256:7d24c7d528c0e5979ad1145886c6131fccb58fafc51b0e151913ed834517786c AS builder
ARG STEEL_WORLDGEN_BUILD_ID
ARG STEEL_WORLDGEN_SOURCE_URL
RUN test -n "${STEEL_WORLDGEN_BUILD_ID}" && test -n "${STEEL_WORLDGEN_SOURCE_URL}"
WORKDIR /src
COPY . .
RUN tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
      --format=posix --pax-option=delete=atime,delete=ctime \
      --exclude=target --exclude=.git --exclude=minecraft-src \
      --exclude=artifacts --exclude='integration/*/build' \
      --exclude='integration/*/target' -cf /steelmc-corresponding-source.tar .
RUN STEEL_WORLDGEN_BUILD_ID="${STEEL_WORLDGEN_BUILD_ID}" \
    cargo build --release --locked -p steel-worldgen-service \
      --bin steel-worldgen-service --bin steel-worldgen-probe
RUN cargo metadata --locked --format-version 1 > /steelmc-cargo-metadata.json && \
    cd /usr/local/cargo && \
    find registry/src git/checkouts -type f \
      \( -iname 'license*' -o -iname 'copying*' -o -iname 'notice*' \) -print0 \
      | LC_ALL=C sort -z \
      | tar --null --no-recursion --mtime=@0 \
          --owner=0 --group=0 --numeric-owner --format=pax \
          --pax-option=delete=atime,delete=ctime \
          -cf /steelmc-third-party-licenses.tar --files-from=-

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:fccdbb0a547c14e23fcf4ce8ad62ca5d43b4faae8d22cd292f490fef9946c96e
ARG STEEL_WORLDGEN_BUILD_ID
ARG STEEL_WORLDGEN_SOURCE_URL
LABEL org.opencontainers.image.source="${STEEL_WORLDGEN_SOURCE_URL}" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.revision="${STEEL_WORLDGEN_BUILD_ID}"
ENV STEEL_WORLDGEN_SOURCE_URL="${STEEL_WORLDGEN_SOURCE_URL}"
COPY --from=builder /src/LICENSE /LICENSE
COPY --from=builder /steelmc-corresponding-source.tar /steelmc-corresponding-source.tar
COPY --from=builder /steelmc-cargo-metadata.json /steelmc-cargo-metadata.json
COPY --from=builder /steelmc-third-party-licenses.tar /steelmc-third-party-licenses.tar
COPY --from=builder /src/target/release/steel-worldgen-service /steel-worldgen-service
COPY --from=builder /src/target/release/steel-worldgen-probe /steel-worldgen-probe
EXPOSE 50051
USER nonroot:nonroot
ENTRYPOINT ["/steel-worldgen-service"]
