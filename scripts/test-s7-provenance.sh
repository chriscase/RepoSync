#!/usr/bin/env bash
# The offline suite exercises the same matcher used by the live scenario.
set -euo pipefail
exec python3 "$(dirname "$0")/test_s7_provenance.py" "$@"
