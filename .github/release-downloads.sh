#!/usr/bin/env bash
# Release notes section listing the downloads in dist/ (from build.yml), with
# links to the release's assets. Usage: release-downloads.sh <tag>
# (GITHUB_REPOSITORY is set by GitHub Actions.)
set -euo pipefail
tag=$1

echo "### Downloads"
echo
echo "| Platform | Download |"
echo "| --- | --- |"
for row in "windows-x64.zip|Windows x64" "windows-x64-server.zip|Windows x64 (server)" \
    "linux-x64.tar.gz|Linux x64" "linux-x64-server.tar.gz|Linux x64 (server)"; do
    suffix=${row%%|*}
    platform=${row#*|}
    for file in dist/*-"$suffix"; do
        [ -e "$file" ] || continue
        name=$(basename "$file")
        size=$(awk -v bytes="$(stat -c %s "$file")" 'BEGIN { printf "%.1f MB", bytes / 1048576 }')
        echo "| $platform | [Download](https://github.com/$GITHUB_REPOSITORY/releases/download/$tag/$name) ($size) |"
    done
done
# The Docker image of this build (see docker-publish.sh): dev builds are `dev`.
case $tag in dev-*) image_tag=dev ;; *) image_tag=${tag#v} ;; esac
cat <<EOF

- **Desktop app:** run \`local-playlist\`. Keep \`local-playlist-login\` next to it: it's the
  window for logging in to YouTube / SoundCloud (on Linux it needs WebKitGTK 4.1).
- **Server:** \`local-playlist --server\` serves the web version (UI included) for browsers;
  see "Web version" in the README. Docker: \`docker pull ghcr.io/${GITHUB_REPOSITORY,,}:$image_tag\`
- Linux builds need glibc 2.39 or newer (e.g. Ubuntu 24.04, Debian 13, Fedora 40).
EOF
