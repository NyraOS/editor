#!/bin/sh
# SPDX-License-Identifier: MIT
# Builds GNU nano 8.4, the reference of the comparison tests (tests/compare), from the GNU release
# tarball, and installs it under PREFIX: PREFIX/bin/nano. CI caches the result.
#
# The tarball is pinned by its SHA-256, taken from a tarball whose GnuPG signature
# (nano-8.4.tar.xz.sig) verified against the GNU keyring, made by the nano maintainer's key
# 168E 6F42 97BF D7A7 9AFD  4496 514B BE2E B8E1 961F. Any mirror will do: the hash is the check.
#
# Needs curl, a C compiler, make and the ncurses headers (Debian and Ubuntu: libncurses-dev).
# Usage: tools/build-nano.sh PREFIX
set -eu

version=8.4
sha256=5ad29222bbd55624d87ea677928b3106a743114d6c6f9b41f36c97be2a8e628d
urls="https://ftpmirror.gnu.org/nano/nano-$version.tar.xz
https://ftp.gnu.org/gnu/nano/nano-$version.tar.xz
https://www.nano-editor.org/dist/v8/nano-$version.tar.xz"

[ $# -eq 1 ] || { echo "usage: $0 PREFIX" >&2; exit 2; }
mkdir -p "$1"
prefix=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for url in $urls; do
    if curl -fsSL --retry 2 --max-time 120 -o "$work/nano.tar.xz" "$url"; then
        break
    fi
    echo "download failed: $url" >&2
done
echo "$sha256  $work/nano.tar.xz" | sha256sum -c -

tar -xJf "$work/nano.tar.xz" -C "$work"
cd "$work/nano-$version"
# No translations: the tests run in English (C.UTF-8). The system nanorc is $prefix/etc/nanorc,
# empty, so that the tests control it (the host's /etc/nanorc is never read).
./configure --quiet --prefix="$prefix" --sysconfdir="$prefix/etc" --disable-nls
make --quiet -j"$(nproc)"
make --quiet install
mkdir -p "$prefix/etc"
: > "$prefix/etc/nanorc"

found=$("$prefix/bin/nano" --version | head -n 1)
case $found in
    *"GNU nano, version $version") echo "built: $found ($prefix/bin/nano)" ;;
    *) echo "unexpected version: $found" >&2; exit 1 ;;
esac
