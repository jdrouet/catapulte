lint:
	rustfmt --edition 2018 --check src/*.rs
	rustfmt --edition 2018 --check src/**/*.rs

test:
	cargo test
