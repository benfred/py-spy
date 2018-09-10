#!/usr/bin/env bash
apt-get update
apt-get -y install python3-pip curl musl-tools
pip3 install wheel
curl https://sh.rustup.rs -sSf | sh -s -- -y
source $HOME/.cargo/env
rustup target add i686-unknown-linux-musl
