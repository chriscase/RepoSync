#!/usr/bin/env bash
# Classify a release tag before anything is published.
#
#   vMAJOR.MINOR.PATCH                    -> stable: may become the latest
#                                            release and the Docker `latest`.
#   vMAJOR.MINOR.PATCH-(alpha|beta|rc).N  -> prerelease: never latest, and the
#                                            image is published under the exact
#                                            version only.
#
# Any other tag is refused, so an unrecognized name can never move a stable
# pointer by falling through to the stable path.
#
# Usage: release_channel.sh <tag>
# Prints `channel=`, `prerelease=` and `make_latest=` lines (GITHUB_OUTPUT form).
set -euo pipefail

tag="${1:-}"
number='(0|[1-9][0-9]*)'
stable="^v${number}\.${number}\.${number}$"
prerelease="^v${number}\.${number}\.${number}-(alpha|beta|rc)\.${number}$"

if [[ "$tag" =~ $stable ]]; then
  printf 'channel=stable\nprerelease=false\nmake_latest=true\n'
elif [[ "$tag" =~ $prerelease ]]; then
  printf 'channel=prerelease\nprerelease=true\nmake_latest=false\n'
else
  echo "release_channel: refusing unrecognized release tag '${tag}'" >&2
  exit 1
fi
