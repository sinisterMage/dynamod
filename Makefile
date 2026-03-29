.PHONY: all zig rust rust-sdnotify clean install install-alpine test test-zig test-rust test-qemu test-alpine test-dbus fmt

DESTDIR ?=
PREFIX  ?= /usr

ZIG_OUT   := zig/zig-out/bin
CARGO_OUT := $(shell if [ -d rust/target/x86_64-unknown-linux-musl/release ]; then echo rust/target/x86_64-unknown-linux-musl/release; else echo rust/target/release; fi)
# dynamod-sdnotify builds as a cdylib (libsystemd.so) and needs the glibc target
SDNOTIFY_OUT := rust/target/x86_64-unknown-linux-gnu/release

all: zig rust

zig:
	cd zig && zig build

rust:
	cd rust && cargo build --release

# Build the libsystemd.so shim separately (requires glibc target, not musl)
rust-sdnotify:
	cd rust/dynamod-sdnotify && cargo build --release --target x86_64-unknown-linux-gnu

clean:
	rm -rf zig/zig-out zig/.zig-cache
	cd rust && cargo clean

install: all
	install -Dm755 $(ZIG_OUT)/dynamod-init      $(DESTDIR)$(PREFIX)/sbin/dynamod-init
	install -Dm755 $(CARGO_OUT)/dynamod-svmgr   $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-svmgr
	install -Dm755 $(CARGO_OUT)/dynamodctl       $(DESTDIR)$(PREFIX)/bin/dynamodctl
	install -Dm755 $(CARGO_OUT)/dynamod-logd     $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-logd
	install -Dm755 $(CARGO_OUT)/dynamod-logind      $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-logind
	install -Dm755 $(CARGO_OUT)/dynamod-sd1bridge   $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-sd1bridge
	install -Dm755 $(CARGO_OUT)/dynamod-hostnamed   $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-hostnamed
	install -dm755 $(DESTDIR)/etc/dynamod/services
	install -dm755 $(DESTDIR)/etc/dynamod/supervisors
	install -Dm644 config/supervisors/root.toml  $(DESTDIR)/etc/dynamod/supervisors/root.toml
	install -dm755 $(DESTDIR)/usr/share/dbus-1/system.d
	install -Dm644 config/dbus-1/*.conf $(DESTDIR)/usr/share/dbus-1/system.d/

# Install libsystemd.so shim (only if built with `make rust-sdnotify`)
install-sdnotify: rust-sdnotify
	install -Dm755 $(SDNOTIFY_OUT)/libsystemd.so $(DESTDIR)$(PREFIX)/lib/libsystemd.so.0.0.0
	ln -sf libsystemd.so.0.0.0 $(DESTDIR)$(PREFIX)/lib/libsystemd.so.0
	ln -sf libsystemd.so.0     $(DESTDIR)$(PREFIX)/lib/libsystemd.so

install-alpine: install
	install -Dm644 config/supervisors/early-boot.toml $(DESTDIR)/etc/dynamod/supervisors/early-boot.toml
	install -Dm644 config/supervisors/desktop.toml    $(DESTDIR)/etc/dynamod/supervisors/desktop.toml
	for f in config/services/*.toml; do \
		install -Dm644 "$$f" "$(DESTDIR)/etc/dynamod/services/$$(basename $$f)"; \
	done

test: test-zig test-rust

test-zig:
	cd zig && zig build test

test-rust:
	cd rust && cargo test --workspace

test-qemu: all
	test/qemu/run-vm.sh

test-alpine: all
	test/alpine/build-test.sh

test-dbus: all
	test/alpine/test-dbus.sh

test-disk: all
	test/alpine/boot-disk.sh

fmt:
	cd rust && cargo fmt --all
