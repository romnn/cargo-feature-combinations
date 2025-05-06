#!/usr/bin/env bash

set -x

echo 'deb [trusted=yes] https://repo.goreleaser.com/apt/ /' | tee /etc/apt/sources.list.d/goreleaser.list
apt update && apt upgrade
apt install -y git goreleaser mingw-w64

# github actions requires to mark the current git repository as safe
git config --global --add safe.directory "$(pwd)"
