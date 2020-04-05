#!/usr/bin/env bash
cd /code/py-spy
export PYSPY_CROSS_COMPILE_TARGET=x86_64-unknown-linux-musl
export PATH=/usr/local/lib/nodejs/node-v12.16.1-linux-x64/bin:$PATH
python3 setup.py bdist_wheel
