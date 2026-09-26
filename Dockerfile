# OpenTrack: one image, one binary; the role is the command (serve, writer, all, ...).

FROM node:22-bookworm-slim AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
COPY ui/vendor ./vendor
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

FROM rust:1-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# The plugin interface the WebAssembly host is generated from.
COPY wit ./wit
RUN cargo build --release --locked --bin opentrack

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home opentrack
COPY --from=server /src/target/release/opentrack /usr/local/bin/opentrack
COPY --from=ui /ui/dist /opt/opentrack/ui
COPY profiles /opt/opentrack/profiles
ENV OT_UI_DIR=/opt/opentrack/ui \
    OT_PROFILES_DIR=/opt/opentrack/profiles/trackers \
    OT_SQLITE_PATH=/data/opentrack.db
USER opentrack
WORKDIR /home/opentrack
VOLUME /data
EXPOSE 8090
ENTRYPOINT ["opentrack"]
CMD ["all"]
