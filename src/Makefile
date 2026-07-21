.PHONY: build dist clean-dist

BIN_NAME := pactience
DIST_DIR := ../dist
RELEASE_BIN := target/release/$(BIN_NAME)

build:
	cargo build --release

dist: build
	mkdir -p $(DIST_DIR)
	cp $(RELEASE_BIN) $(DIST_DIR)/

clean-dist:
	rm -rf $(DIST_DIR)
