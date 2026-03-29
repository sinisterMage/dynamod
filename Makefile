.PHONY: all zig rust clean install test test-zig test-rust test-qemu fmt

DESTDIR ?=
PREFIX  ?= /usr/local

ZIG_OUT   := zig/zig-out/bin
CARGO_OUT := $(shell if [ -d rust/target/x86_64-unknown-linux-musl/release ]; then echo rust/target/x86_64-unknown-linux-musl/release; else echo rust/target/release; fi)

all: zig rust

zig:
	cd zig && zig build

rust:
	cd rust && cargo build --release

clean:
	rm -rf zig/zig-out zig/.zig-cache
	cd rust && cargo clean

install: all
	install -Dm755 $(ZIG_OUT)/dynamod-init      $(DESTDIR)$(PREFIX)/sbin/dynamod-init
	install -Dm755 $(CARGO_OUT)/dynamod-svmgr   $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-svmgr
	install -Dm755 $(CARGO_OUT)/dynamodctl       $(DESTDIR)$(PREFIX)/bin/dynamodctl
	install -Dm755 $(CARGO_OUT)/dynamod-logd     $(DESTDIR)$(PREFIX)/lib/dynamod/dynamod-logd
	install -dm755 $(DESTDIR)/etc/dynamod/services
	install -dm755 $(DESTDIR)/etc/dynamod/supervisors
	install -Dm644 config/supervisors/root.toml  $(DESTDIR)/etc/dynamod/supervisors/root.toml

test: test-zig test-rust

test-zig:
	cd zig && zig build test

test-rust:
	cd rust && cargo test --workspace

test-qemu: all
	test/qemu/run-vm.sh

fmt:
	cd rust && cargo fmt --all
