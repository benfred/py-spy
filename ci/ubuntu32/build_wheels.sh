#!/usr/bin/env bash
cd /code/py-spy
export PYSPY_MUSL_32=1
python3 setup.py bdist_wheel
