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

.PHONY: all build install uninstall clean

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
