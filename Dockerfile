# Build the console separately so the Rust image only receives web/dist.
FROM node:22-bookworm-slim AS web-build

WORKDIR /src/web
RUN npm install --global pnpm@10

COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY web/ ./
RUN pnpm build

# The tag's Rust is not what compiles: a rust image names its toolchain after its
# version, while rust-toolchain.toml asks for "stable", so rustup fetches stable
# no matter which tag this is. Hence a floating tag, and one explicit install in
# the stage every build stage inherits from -- left implicit, planner and
# hub-build each fetch their own copy.
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /src

# The component list mirrors rust-toolchain.toml so that nothing is left for
# rustup to fetch once cargo starts.
RUN rustup toolchain install stable --profile minimal --component clippy

# recipe.json describes only the dependency graph, so the expensive cook layer
# below stays cached until Cargo.toml or Cargo.lock actually changes.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS hub-build
COPY --from=planner /src/recipe.json recipe.json

# build.rs refuses to compile embed-web without a console build. A stub satisfies
# it while keeping this layer keyed on the dependency graph alone, so editing the
# console does not invalidate the dependency cache.
RUN mkdir -p web/dist && touch web/dist/index.html
RUN cargo chef cook --profile release-lto -p nagisalake-hub \
    --features embed-web --recipe-path recipe.json

# Only the workspace crates compile past this point. .dockerignore keeps target/
# out of the context so the cooked artifacts survive this COPY.
COPY . .
COPY --from=web-build /src/web/dist ./web/dist

RUN cargo build --profile release-lto -p nagisalake-hub --features embed-web

# The runtime image contains no source tree, package manager, or secrets.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 nagisalake \
    && useradd --system --uid 10001 --gid 10001 --home-dir /nonexistent \
       --no-create-home nagisalake \
    && mkdir -p /etc/nagisalake \
    && chown nagisalake:nagisalake /etc/nagisalake

COPY --from=hub-build /src/target/release-lto/nagisalake-hub /usr/local/bin/nagisalake-hub

USER nagisalake:nagisalake
EXPOSE 9091
STOPSIGNAL SIGTERM

# Mount a runtime config at /etc/nagisalake/hub.toml. It must listen on
# 0.0.0.0:9091 inside the container; inject secrets as environment variables.
ENTRYPOINT ["/usr/local/bin/nagisalake-hub"]
CMD ["--config", "/etc/nagisalake/hub.toml"]
