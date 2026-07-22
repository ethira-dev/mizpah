#!/usr/bin/env bash
# Rewrite Formula/mizpah.rb in ethira-dev/homebrew-tap for a published release.
#
# Required env:
#   VERSION          e.g. 0.19.0 (no leading v)
#   HOMEBREW_TAP_TOKEN  GitHub token with contents:write on ethira-dev/homebrew-tap
# Optional:
#   TAP_REPO         default ethira-dev/homebrew-tap
#   RELEASE_REPO     default ethira-dev/mizpah
#   DIST_DIR         directory containing mizpah-*.tar.gz archives (default: dist)
set -euo pipefail

VERSION="${VERSION:?VERSION is required (e.g. 0.19.0)}"
VERSION="${VERSION#v}"
TAG="v${VERSION}"
TAP_REPO="${TAP_REPO:-ethira-dev/homebrew-tap}"
RELEASE_REPO="${RELEASE_REPO:-ethira-dev/mizpah}"
DIST_DIR="${DIST_DIR:-dist}"
TOKEN="${HOMEBREW_TAP_TOKEN:?HOMEBREW_TAP_TOKEN is required}"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

need_asset() {
  local name="$1"
  local path="${DIST_DIR}/${name}"
  [[ -f "${path}" ]] || {
    echo "missing release asset: ${path}" >&2
    exit 1
  }
  echo "${path}"
}

ARM_DARWIN="$(need_asset "mizpah-aarch64-apple-darwin.tar.gz")"
INTEL_DARWIN="$(need_asset "mizpah-x86_64-apple-darwin.tar.gz")"
LINUX_X64="$(need_asset "mizpah-x86_64-unknown-linux-gnu.tar.gz")"

SHA_ARM="$(sha256_file "${ARM_DARWIN}")"
SHA_INTEL="$(sha256_file "${INTEL_DARWIN}")"
SHA_LINUX="$(sha256_file "${LINUX_X64}")"

BASE="https://github.com/${RELEASE_REPO}/releases/download/${TAG}"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

git clone --depth 1 "https://x-access-token:${TOKEN}@github.com/${TAP_REPO}.git" "${TMP}/tap"
cd "${TMP}/tap"

cat > Formula/mizpah.rb <<EOF
class Mizpah < Formula
  desc "JSON log viewer with web UI and MCP for AI agents"
  homepage "https://github.com/${RELEASE_REPO}"
  version "${VERSION}"
  license "MIT"

  on_macos do
    on_arm do
      url "${BASE}/mizpah-aarch64-apple-darwin.tar.gz"
      sha256 "${SHA_ARM}"
    end
    on_intel do
      url "${BASE}/mizpah-x86_64-apple-darwin.tar.gz"
      sha256 "${SHA_INTEL}"
    end
  end

  on_linux do
    on_intel do
      url "${BASE}/mizpah-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${SHA_LINUX}"
    end
  end

  def install
    bin.install "mizpah"
    bin.install "mzp"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mizpah --version")
  end
end
EOF

git config user.name "ethira-release-bot"
git config user.email "noreply@ethira.dev"

if git diff --quiet Formula/mizpah.rb; then
  echo "Formula/mizpah.rb already at ${VERSION}; nothing to commit"
  exit 0
fi

git add Formula/mizpah.rb
git commit -m "mizpah ${VERSION}"
git push origin HEAD:main
echo "Updated ${TAP_REPO} Formula/mizpah.rb to ${VERSION}"
