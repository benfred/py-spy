#!/usr/bin/env bash

source ~/.bash_profile

set -e

python --version
cargo --version

export CARGO_HOME="/vagrant/.cargo"
mkdir -p $CARGO_HOME

cd /vagrant

if [ -f build-artifacts.tar ]; then
  tar xf build-artifacts.tar
  rm -f build-artifacts.tar
fi

cargo build --release --workspace --all-targets
cargo test --release

tar cf build-artifacts.tar target
