#!/usr/bin/env bash

set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
makepkg -sic
rm -f -- line-rust-*.tar.gz line-rust-*.pkg.tar.*
