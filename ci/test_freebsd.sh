#!/usr/bin/env bash

source "$HOME/.cargo/env"

set -e

python --version
cargo --version

cd /vagrant

if [ -f build-artifacts.tar ]; then
  echo "Unpacking cached build artifacts..."
  tar xf build-artifacts.tar
  rm -f build-artifacts.tar
fi

cargo build --release --workspace --all-targets
cargo test --release

set +e
tar cf build-artifacts.tar target
tar rf build-artifacts.tar "$HOME/.cargo/git"
tar rf build-artifacts.tar "$HOME/.cargo/registry"

exit 0
