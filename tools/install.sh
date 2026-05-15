#!/bin/sh
# Install dynamod into $DESTDIR$PREFIX. Invoked by `neomake run install`.
set -eu

: "${DESTDIR:=}"
: "${PREFIX:=/usr}"

ZIG_OUT="zig/zig-out/bin"
if [ -d rust/target/x86_64-unknown-linux-musl/release ]; then
    CARGO_OUT="rust/target/x86_64-unknown-linux-musl/release"
else
    CARGO_OUT="rust/target/release"
fi

install -Dm755 "$ZIG_OUT/dynamod-init"        "$DESTDIR$PREFIX/sbin/dynamod-init"
install -Dm755 "$CARGO_OUT/dynamod-svmgr"     "$DESTDIR$PREFIX/lib/dynamod/dynamod-svmgr"
install -Dm755 "$CARGO_OUT/dynamodctl"        "$DESTDIR$PREFIX/bin/dynamodctl"
install -Dm755 "$CARGO_OUT/dynamod-logd"      "$DESTDIR$PREFIX/lib/dynamod/dynamod-logd"
install -Dm755 "$CARGO_OUT/dynamod-logind"    "$DESTDIR$PREFIX/lib/dynamod/dynamod-logind"
install -Dm755 "$CARGO_OUT/dynamod-sd1bridge" "$DESTDIR$PREFIX/lib/dynamod/dynamod-sd1bridge"
install -Dm755 "$CARGO_OUT/dynamod-hostnamed" "$DESTDIR$PREFIX/lib/dynamod/dynamod-hostnamed"

install -dm755 "$DESTDIR/etc/dynamod/services"
install -dm755 "$DESTDIR/etc/dynamod/supervisors"
install -Dm644 config/supervisors/root.toml "$DESTDIR/etc/dynamod/supervisors/root.toml"

install -dm755 "$DESTDIR/usr/share/dbus-1/system.d"
for f in config/dbus-1/*.conf; do
    install -Dm644 "$f" "$DESTDIR/usr/share/dbus-1/system.d/$(basename "$f")"
done
