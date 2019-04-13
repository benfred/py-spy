#!/usr/bin/env bash
cd /code/py-spy
export PYSPY_CROSS_COMPILE_TARGET=i686-unknown-linux-musl
python3 setup.py bdist_wheel
