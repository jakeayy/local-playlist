#!/usr/bin/env bash
# Pushes the server image to GitHub's container registry, ghcr.io/<owner>/<repo>:<tag>
# for each tag given. The image is the Dockerfile's `prebuilt` stage around the Linux
# server download in dist/ (from build.yml), so it's the released binary.
# Usage: docker-publish.sh <version label> <tag>...
# Needs GH_TOKEN with packages: write. (GITHUB_REPOSITORY, GITHUB_ACTOR and GITHUB_SHA
# are set by GitHub Actions.)
set -euo pipefail
version=$1
shift
image=ghcr.io/${GITHUB_REPOSITORY,,}

archives=(dist/*-linux-x64-server.tar.gz)
[ "${#archives[@]}" -eq 1 ] && [ -e "${archives[0]}" ] || { echo "expected one Linux server archive in dist/" >&2; exit 1; }
context=$(mktemp -d)
tar -xzf "${archives[0]}" -C "$context" --strip-components=1

tags=()
for tag; do tags+=(--tag "$image:$tag"); done
docker build --target prebuilt --file Dockerfile "${tags[@]}" \
    --label org.opencontainers.image.version="$version" \
    --label org.opencontainers.image.revision="$GITHUB_SHA" \
    "$context"

echo "$GH_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin
for tag; do docker push "$image:$tag"; done
