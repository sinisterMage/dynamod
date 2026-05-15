#!/bin/sh
# Install Alpine-specific supervisor/service configs on top of the base
# install. Invoked by `neomake run install-alpine`.
set -eu

: "${DESTDIR:=}"

install -Dm644 config/supervisors/early-boot.toml "$DESTDIR/etc/dynamod/supervisors/early-boot.toml"
install -Dm644 config/supervisors/desktop.toml    "$DESTDIR/etc/dynamod/supervisors/desktop.toml"

for f in config/services/*.toml; do
    install -Dm644 "$f" "$DESTDIR/etc/dynamod/services/$(basename "$f")"
done
