# contacts-sync image: a static musl binary on an empty base (no shell, no package manager).
# Built by .github/workflows/image.yml and pushed to ghcr.io/tywrr/contacts-sync.

FROM docker.io/library/rust:1.99.1-alpine3.22@sha256:731f64e16c12af70970d9a3655eecb1eecab4898352f09688a8e5ca2ab8b77c5 AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# Dependencies first, so editing src/ does not re-download crates on every build.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src
COPY src ./src
# The image build fails if any unit test fails.
RUN touch src/main.rs && cargo test --release --locked && cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/contacts-sync /contacts-sync
USER 65534:65534
ENTRYPOINT ["/contacts-sync"]
