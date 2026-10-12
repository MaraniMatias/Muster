#!/usr/bin/env bash
set -euo pipefail

if [[ "$(id -u)" == 0 ]]; then
  sudo=()
else
  command -v sudo >/dev/null || { echo "sudo is required to install Linux build prerequisites" >&2; exit 1; }
  sudo=(sudo)
fi
"${sudo[@]}" apt-get update
"${sudo[@]}" apt-get install -y \
  build-essential \
  curl \
  file \
  libayatana-appindicator3-dev \
  libfuse2 \
  libgtk-3-0 \
  librsvg2-dev \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  pkg-config \
  zsh \
  wget