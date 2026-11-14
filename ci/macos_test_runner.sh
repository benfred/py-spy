#!/usr/bin/env bash
set -euo pipefail

# Keep setup-python's interpreter available to tests that spawn Python children.
exec sudo "PATH=$PATH" "$@"
