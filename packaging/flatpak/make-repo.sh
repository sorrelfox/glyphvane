#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Glyphvane contributors

# Builds the Glyphvane Flatpak repository site from release bundles: a fresh
# GPG-signed OSTree repository in <site>/repo/, the .flatpakrepo and
# .flatpakref files that point to it, the public key and an index.html.
# The flatpak-repo workflow runs it and deploys <site> to GitHub Pages.
#
# Usage:
#   GPG_HOMEDIR=<gnupg dir with the private key> \
#     packaging/flatpak/make-repo.sh <site> <bundle.flatpak>...
#
# Environment:
#   GPG_HOMEDIR    GnuPG home that holds the private key of
#                  packaging/flatpak/glyphvane-repo.gpg (required)
#   SITE_URL       where <site> is served (default: the GitHub Pages URL)
#
# The signing key is the one in glyphvane-repo.gpg (the public key that
# bundles and users trust), so the published files cannot drift from it.

set -euo pipefail

APP_ID=io.github.sorrelfox.Glyphvane
SITE_URL=${SITE_URL:-https://sorrelfox.github.io/glyphvane/}
SITE_URL=${SITE_URL%/}/
REPO_URL=${SITE_URL}repo/
RUNTIME_REPO=https://dl.flathub.org/repo/flathub.flatpakrepo
HOMEPAGE=https://github.com/sorrelfox/glyphvane
here=$(cd "$(dirname "$0")" && pwd)
pubkey=$here/glyphvane-repo.gpg

die() {
  echo "make-repo.sh: $*" >&2
  exit 1
}

[ $# -ge 2 ] || die "usage: GPG_HOMEDIR=<dir> $0 <site> <bundle.flatpak>..."
[ -n "${GPG_HOMEDIR:-}" ] || die "GPG_HOMEDIR is not set"
[ -f "$pubkey" ] || die "missing $pubkey"
site=$1
shift
for bundle in "$@"; do
  [ -f "$bundle" ] || die "no such bundle: $bundle"
done

# The fingerprint of the committed public key; the private key for it must
# be in GPG_HOMEDIR.
fingerprint=$(gpg --batch --with-colons --show-keys "$pubkey" | awk -F: '$1 == "fpr" { print $10; exit }')
[ -n "$fingerprint" ] || die "cannot read the key in $pubkey"
gpg --batch --homedir "$GPG_HOMEDIR" --list-secret-keys "$fingerprint" > /dev/null 2>&1 ||
  die "GPG_HOMEDIR has no private key for $fingerprint (packaging/flatpak/glyphvane-repo.gpg)"
gpg_key_b64=$(base64 -w0 "$pubkey")

rm -rf "$site"
mkdir -p "$site"
repo=$site/repo
ostree init --repo="$repo" --mode=archive-z2

for bundle in "$@"; do
  echo "Importing $bundle"
  flatpak build-import-bundle --gpg-sign="$fingerprint" --gpg-homedir="$GPG_HOMEDIR" \
    "$repo" "$bundle"
done

flatpak build-update-repo --gpg-sign="$fingerprint" --gpg-homedir="$GPG_HOMEDIR" \
  --title="Glyphvane" --homepage="$HOMEPAGE" \
  --comment="Glyphvane, a Markdown reader and editor" \
  --generate-static-deltas --prune "$repo"

# The branch the bundles carry (flatpak-builder's default, master).
branch=$(ostree refs --repo="$repo" | awk -F/ -v id="$APP_ID" '$1 == "app" && $2 == id { print $4; exit }')
[ -n "$branch" ] || die "the bundles do not contain app/$APP_ID"

cat > "$site/glyphvane.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Glyphvane
Url=$REPO_URL
Homepage=$HOMEPAGE
Comment=Glyphvane, a Markdown reader and editor
Description=Glyphvane releases for Linux (x86_64 and aarch64)
GPGKey=$gpg_key_b64
EOF

# One .flatpakref under two names: the short one for the documentation, the
# app ID one for software centres that expect it.
cat > "$site/$APP_ID.flatpakref" <<EOF
[Flatpak Ref]
Name=$APP_ID
Branch=$branch
Title=Glyphvane
Url=$REPO_URL
SuggestRemoteName=glyphvane
Homepage=$HOMEPAGE
Comment=Glyphvane, a Markdown reader and editor
RuntimeRepo=$RUNTIME_REPO
IsRuntime=false
GPGKey=$gpg_key_b64
EOF
cp "$site/$APP_ID.flatpakref" "$site/glyphvane.flatpakref"
cp "$pubkey" "$site/glyphvane-repo.gpg"

fpr_spaced=$(echo "$fingerprint" | sed -E 's/(.{4})/\1 /g; s/ $//')
cat > "$site/index.html" <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Glyphvane Flatpak repository</title>
</head>
<body>
<h1>Glyphvane Flatpak repository</h1>
<p>Install <a href="$HOMEPAGE">Glyphvane</a> from this repository; Flatpak then
keeps it up to date with <code>flatpak update</code>. The freedesktop runtime
comes from Flathub, which is added if it is missing.</p>
<pre>flatpak install ${SITE_URL}glyphvane.flatpakref</pre>
<p>Or add the repository and install from it:</p>
<pre>flatpak remote-add --if-not-exists glyphvane ${SITE_URL}glyphvane.flatpakrepo
flatpak install glyphvane $APP_ID</pre>
<p>The repository is signed with the key
<a href="glyphvane-repo.gpg">glyphvane-repo.gpg</a>, fingerprint
<code>$fpr_spaced</code>.</p>
</body>
</html>
EOF

echo "Repository refs:"
ostree refs --repo="$repo" | sort
