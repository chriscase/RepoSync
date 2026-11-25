#!/usr/bin/env bash
# The offline suite exercises the same preflight and matcher as the live scenario.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
bash "$script_dir/test_s7_prepare_canary_pr.sh"
exec python3 "$script_dir/test_s7_provenance.py" "$@"
