#!/bin/bash
# base.sh: runs as root on a fresh Boat box (Ubuntu, amd64) when `perseids image build` bakes an image. It adds what
# lib/runner.rb needs (ruby, git, Docker for services:, mise for setup-* steps) and proves each one works. Your repo's
# own needs go in .perseids/setup.sh, which runs after this, also as root. Put no secrets in an image.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y curl ca-certificates git jq ruby build-essential unzip xz-utils
command -v docker >/dev/null || apt-get install -y docker.io
systemctl enable --now docker 2>/dev/null || true
id -nG user | grep -qw docker || usermod -aG docker user
command -v mise >/dev/null || curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh

# Prove the tools, not just their version strings.
ruby -e 'require "yaml"; require "json"; puts RUBY_VERSION'
sudo -u user -H mise --version
sudo -u user -H docker info >/dev/null
git --version
apt-get clean
