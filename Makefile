.PHONY: build release test e2e lint fmt install all clean

build:
	cargo build --workspace

release:
	cargo build --workspace --release

test:
	cargo test --workspace

e2e:
	cargo test --workspace --release --test '*'

lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt

install:
	cargo install --path crates/musts --locked

all: lint test e2e

clean:
	cargo clean
