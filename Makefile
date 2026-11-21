# sh_web_search_tool_rs — HTTP + MCP search service
#
# make build          release build (dynamic, glibc)
# make build-static   fully static release build (x86_64-unknown-linux-musl)
# make install        build + install to /usr/local/bin + systemd service
# make uninstall      remove binary + systemd service

BINARY       := sh_web_search_tool_rs
INSTALL_NAME := web-search
MUSL_TARGET  := x86_64-unknown-linux-musl
STATIC_BIN   := target/$(MUSL_TARGET)/release/$(BINARY)
RELEASE_BIN  := target/release/$(BINARY)
SERVICE      := web-search.service
IS_ROOT      := $(shell [ "$$(id -u)" -eq 0 ] && echo 1)
SUDO         := $(if $(IS_ROOT),,sudo)

.PHONY: help build build-static run test check fmt clippy install uninstall clean

help:
	@echo "targets: build build-static run test check fmt clippy install uninstall clean"

## Release build
build:
	cargo build --release

## Fully static build (static-pie, no external dependencies): the TLS stack is
## rustls + ring, so OpenSSL is not needed. `ring` requires a C compiler that
## can target musl — install `musl-tools` (musl-gcc) for a fully musl-native
## toolchain; otherwise the host cc is used for the C parts.
## Note: `-C target-feature=+crt-static` is applied only for the musl target —
## on x86_64-unknown-linux-gnu it breaks proc-macro crates (serde_derive,
## async-trait, rmcp-macros).
build-static:
	@rustup target list --installed | grep -qx "$(MUSL_TARGET)" \
		|| { echo "missing target: run 'rustup target add $(MUSL_TARGET)'"; exit 1; }
	@cc="$$(command -v x86_64-linux-musl-gcc 2>/dev/null || command -v musl-gcc 2>/dev/null || command -v cc)"; \
	 ar="$$(command -v x86_64-linux-musl-ar 2>/dev/null || command -v ar)"; \
	 echo "C compiler for $(MUSL_TARGET): $$cc"; \
	 CC_x86_64_unknown_linux_musl="$$cc" AR_x86_64_unknown_linux_musl="$$ar" \
	 cargo build --release --target $(MUSL_TARGET)
	@file $(STATIC_BIN)

## Build and run both servers (HTTP on :3333 + MCP on stdio)
run: build
	./$(RELEASE_BIN)

test:
	cargo test

## Verify: fmt + clippy + tests
check: fmt clippy
	@cargo test

## Format sources
fmt:
	cargo fmt --all

## Lint
clippy:
	cargo clippy --all-targets -- -D warnings

## Build, copy the binary to /usr/local/bin and install the systemd unit
install: build
	$(SUDO) ./install.sh install

## Remove the binary and the systemd service
uninstall:
	$(SUDO) ./install.sh uninstall

clean:
	cargo clean
