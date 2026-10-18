FROM ubuntu:22.04
ARG PNPM_VERSION
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl gnupg \
    && rm -rf /var/lib/apt/lists/*
COPY install-linux-deps.sh /usr/local/bin/install-linux-deps
RUN bash /usr/local/bin/install-linux-deps \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && corepack enable \
    && corepack prepare "pnpm@${PNPM_VERSION}" --activate \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none \
    && /root/.cargo/bin/rustup toolchain install 1.97.1 --profile minimal --component clippy --component rustfmt --target x86_64-unknown-linux-gnu \
    && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
WORKDIR /workspace