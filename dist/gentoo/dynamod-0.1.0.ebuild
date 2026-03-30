# Copyright 2026 Ofek Bickel
# Distributed under the terms of the MIT License

EAPI=8

DESCRIPTION="Zig+Rust PID 1 init system with OTP supervisor trees and systemd compatibility"
HOMEPAGE="https://github.com/sinisterMage/dynamod"

# For local install, place the repo at /var/tmp/portage/sys-apps/dynamod-0.1.0/work/dynamod
# or use a local overlay with SRC_URI=""
SRC_URI=""
S="${WORKDIR}/${PN}"

LICENSE="MIT"
SLOT="0"
KEYWORDS="~amd64"

DEPEND="
	dev-lang/zig
	dev-lang/rust
"
RDEPEND="
	sys-apps/dbus
"

src_compile() {
	cd zig && zig build -Doptimize=ReleaseSafe || die "Zig build failed"
	cd "${S}/rust" && cargo build --release || die "Rust build failed"
}

src_test() {
	cd zig && zig build test || die "Zig tests failed"
	cd "${S}/rust" && cargo test --workspace || die "Rust tests failed"
}

src_install() {
	local zigout="zig/zig-out/bin"
	local rustout="rust/target/x86_64-unknown-linux-musl/release"

	# Core binaries
	dosbin "${zigout}/dynamod-init"
	exeinto /usr/lib/dynamod
	doexe "${rustout}/dynamod-svmgr"
	doexe "${rustout}/dynamod-logd"
	dobin "${rustout}/dynamodctl"

	# systemd-mimic binaries
	for bin in dynamod-logind dynamod-sd1bridge dynamod-hostnamed; do
		[ -f "${rustout}/${bin}" ] && doexe "${rustout}/${bin}"
	done

	# Configs
	insinto /etc/dynamod/supervisors
	doins config/supervisors/*.toml

	insinto /etc/dynamod/services
	doins config/services/*.toml

	# D-Bus policy files
	insinto /usr/share/dbus-1/system.d
	doins config/dbus-1/*.conf

	# Runtime dirs
	keepdir /var/lib/dynamod
	keepdir /var/log/dynamod

	# Docs
	dodoc README.md
	dodoc docs/architecture.md
	dodoc docs/configuration.md
}
