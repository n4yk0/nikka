BINARY := nikka
PREFIX ?= $(HOME)/.local

.PHONY: build test lint generate install uninstall clean

build:
	cargo build --release --locked

test:
	cargo test --locked --all-targets

lint:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings

generate:
	cargo run --locked --example logogen -- --in assets/logo.png --out src/logo/grid.txt --width 30

install: build
	install -Dm755 target/release/$(BINARY) $(PREFIX)/bin/$(BINARY)

uninstall:
	rm -f $(PREFIX)/bin/$(BINARY)

clean:
	cargo clean
