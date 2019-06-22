#!/usr/bin/env bash
apt-get update
apt-get -y install python3-pip curl musl-tools
pip3 install wheel
curl https://sh.rustup.rs -sSf | sh -s -- -y
source $HOME/.cargo/env
rustup target add x86_64-unknown-linux-musl

# download libunwind and build a static version w/ musl-gcc
wget https://github.com/libunwind/libunwind/releases/download/v1.3.1/libunwind-1.3.1.tar.gz
tar -zxvf libunwind-1.3.1.tar.gz
cd libunwind-1.3.1/
CC=musl-gcc ./configure --disable-minidebuginfo --enable-ptrace --disable-tests --disable-documentation
make
make install
