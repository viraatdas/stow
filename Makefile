SHELL := /bin/bash

PROJECT  := Stow.xcodeproj
CONFIG   ?= Release
DERIVED  := build
PRODUCTS := $(DERIVED)/Build/Products/$(CONFIG)
DEST     := platform=macOS,arch=arm64

# Local install locations (build-from-source, ad-hoc signed, personal use).
PREFIX   ?= $(HOME)/.local
APPDIR   ?= $(HOME)/Applications

# v1 builds with signing disabled (Apple Silicon still ad-hoc signs, so binaries
# run). The agent/extension's restricted entitlements (app-groups, fileprovider)
# need a provisioning profile to sign — set DEVELOPMENT_TEAM to a (free) personal
# team and flip this to enable the extension to load at runtime (M1).
SIGN := CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO

.PHONY: all gen rust test build agent cli install uninstall clean

all: build

## Generate the Xcode project from project.yml
gen:
	xcodegen generate

## Build the Rust core into StowCore.xcframework
rust:
	./scripts/build-rust-xcframework.sh

## Run the Rust core test suite
test:
	cargo test

## Build the agent (with embedded extension) and the CLI
build: gen agent cli

agent:
	xcodebuild -project $(PROJECT) -scheme StowAgent -configuration $(CONFIG) \
		-destination '$(DEST)' -derivedDataPath $(DERIVED) build $(SIGN)

cli:
	xcodebuild -project $(PROJECT) -scheme stow -configuration $(CONFIG) \
		-destination '$(DEST)' -derivedDataPath $(DERIVED) build $(SIGN)

## Install the agent app + stow CLI locally
install: build
	mkdir -p "$(APPDIR)" "$(PREFIX)/bin"
	rm -rf "$(APPDIR)/StowAgent.app"
	cp -R "$(PRODUCTS)/StowAgent.app" "$(APPDIR)/StowAgent.app"
	ln -sf "$(PREFIX)/bin/stow" /dev/null 2>/dev/null || true
	install "$(PRODUCTS)/stow" "$(PREFIX)/bin/stow"
	@echo ""
	@echo "Installed StowAgent.app -> $(APPDIR) and stow -> $(PREFIX)/bin"
	@echo "Make sure $(PREFIX)/bin is on your PATH, then run: stow init"

uninstall:
	rm -rf "$(APPDIR)/StowAgent.app"
	rm -f "$(PREFIX)/bin/stow"

clean:
	rm -rf $(DERIVED) artifacts/StowCore.xcframework artifacts/.build-checksum
	cargo clean
