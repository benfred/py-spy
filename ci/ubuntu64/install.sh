#!/usr/bin/env bash
apt-get update
apt-get -y install python3-pip curl
pip3 install setuptools_rust
curl https://sh.rustup.rs -sSf | sh -s -- -y
