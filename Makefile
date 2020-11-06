ci-coverage:
	cargo tarpaulin --out Xml
	curl -s https://codecov.io/bash | bash

ci-lint:
	rustfmt --edition 2018 --check src/*.rs
	rustfmt --edition 2018 --check src/**/*.rs

ci-test:
	cargo test

dev-coverage:
	SMTP_PORT=1025 TEMPLATE_ROOT=./template cargo tarpaulin --out Html

dev-env:
	docker-compose -f docker-compose.dev.yml up -d

dev-test:
	SMTP_PORT=1025 TEMPLATE_ROOT=./template cargo test

install-rustfmt:
	rustup component add rustfmt

install-tarpaulin:
	cargo install cargo-tarpaulin

release:
	docker buildx build --push \
		--file multiarch.Dockerfile \
		--platform linux/amd64,linux/i386,linux/arm/v7,linux/arm64 \
		--tag jdrouet/catapulte:latest \
		--tag jdrouet/catapulte:${VERSION} \
		--label org.label-schema.version=${VERSION} \
		--label org.label-schema.vcs-ref=${shell git rev-parse --short HEAD} \
		.
