.PHONY: all build run test clean install uninstall

# User-local install by default (XDG); `sudo make install PREFIX=/usr/local` for system-wide.
PREFIX  ?= $(HOME)/.local
BINDIR  := $(PREFIX)/bin
APPDIR  := $(PREFIX)/share/applications
ICONDIR := $(PREFIX)/share/icons/hicolor/scalable/apps

all: build

build:
	cargo build --release
	mkdir -p bin
	cp --remove-destination target/release/ted bin/ted

run: build
	./bin/ted $(ARGS)

test:
	cargo test --workspace

install: build
	install -Dm755 bin/ted $(DESTDIR)$(BINDIR)/ted
	install -Dm644 packaging/linux/ted.svg $(DESTDIR)$(ICONDIR)/ted.svg
	mkdir -p $(DESTDIR)$(APPDIR)
	sed 's|@BINDIR@|$(BINDIR)|' packaging/linux/ted.desktop.in > $(DESTDIR)$(APPDIR)/ted.desktop
	$(refresh-caches)

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/ted $(DESTDIR)$(ICONDIR)/ted.svg $(DESTDIR)$(APPDIR)/ted.desktop
	$(refresh-caches)

# Launchers pick up new entries on their own; refreshing just makes it immediate. Both tools are optional.
define refresh-caches
	if command -v update-desktop-database >/dev/null; then update-desktop-database -q $(DESTDIR)$(APPDIR); fi
	if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -qtf $(DESTDIR)$(PREFIX)/share/icons/hicolor; fi
endef

clean:
	cargo clean
	rm -rf bin
