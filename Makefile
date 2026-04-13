# yggdra Makefile
# Typical usage:
#   make install          → builds release + installs to ~/.local/bin
#   sudo make install PREFIX=/usr/local → system-wide
#   make uninstall        → removes binary
#   cargo install --path . → pure-Rust alternative (installs to ~/.cargo/bin)

PREFIX  ?= $(HOME)/.local
BINDIR  := $(PREFIX)/bin
BINARY  := yggdra
TARGET  := target/release/$(BINARY)

.PHONY: all build install uninstall clean publish release

all: build

build:
	cargo build --release

install: build
	@mkdir -p $(BINDIR)
	install -m 755 $(TARGET) $(BINDIR)/$(BINARY)
	@echo "✅ Installed to $(BINDIR)/$(BINARY)"
	@echo "   Make sure $(BINDIR) is on your PATH."
	@if ! echo "$$PATH" | grep -q "$(BINDIR)"; then \
		echo "   Add this to your shell profile:"; \
		echo "     export PATH=\"$(BINDIR):\$$PATH\""; \
	fi

uninstall:
	rm -f $(BINDIR)/$(BINARY)
	@echo "🗑️  Removed $(BINDIR)/$(BINARY)"

clean:
	cargo clean

# Publish current version to crates.io (runs tests first)
publish:
	cargo test --lib
	cargo publish

# Bump version, commit, tag, and publish to crates.io
# Usage: make release VERSION=0.2.0
release:
	@if [ -z "$(VERSION)" ]; then echo "❌ Usage: make release VERSION=x.y.z"; exit 1; fi
	@sed -i '' 's/^version = ".*"/version = "$(VERSION)"/' Cargo.toml
	cargo test --lib
	git add Cargo.toml Cargo.lock
	git commit -m "chore: release v$(VERSION)"
	git tag -a "v$(VERSION)" -m "v$(VERSION)"
	git push && git push --tags
	cargo publish
