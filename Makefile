.PHONY: build clean test run fmt install help

build:
	cargo build

clean:
	cargo clean

test:
	cargo test

run:
	cargo run

fmt:
	cargo fmt

install:
	cargo install --path .

help:
	@echo "make build    # Compile the project"
	@echo "make run      # Run raygun in dev mode"
	@echo "make test     # Execute cargo tests"
	@echo "make fmt      # Format with rustfmt"
	@echo "make install  # Install raygun into cargo bin path"
	@echo "make clean    # Remove build artifacts"
