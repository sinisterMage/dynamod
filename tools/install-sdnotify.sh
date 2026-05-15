#!/bin/sh
# Install the libsystemd.so shim. Invoked by `neomake run install-sdnotify`.
# Requires the cdylib to have been built against the glibc target.
set -eu

: "${DESTDIR:=}"
: "${PREFIX:=/usr}"

SDNOTIFY_OUT="rust/target/x86_64-unknown-linux-gnu/release"

install -Dm755 "$SDNOTIFY_OUT/libsystemd.so" "$DESTDIR$PREFIX/lib/libsystemd.so.0.0.0"
ln -sf libsystemd.so.0.0.0 "$DESTDIR$PREFIX/lib/libsystemd.so.0"
ln -sf libsystemd.so.0     "$DESTDIR$PREFIX/lib/libsystemd.so"
