lint:
	rustfmt --edition 2018 --check src/*.rs
	rustfmt --edition 2018 --check src/**/*.rs

test:
	cargo test

release:
	docker buildx build --push \
		--file multiarch.Dockerfile \
		--platform linux/amd64,linux/i386,linux/arm/v7,linux/arm64 \
		--tag jdrouet/catapulte:latest \
		--tag jdrouet/catapulte:${VERSION} \
		--label org.label-schema.version=${VERSION} \
		--label org.label-schema.vcs-ref=${shell git rev-parse --short HEAD} \
		.

dev-env:
	docker-compose -f docker-compose.dev.yml up -d

dev-test:
	SMTP_PORT=1025 cargo test

dev-coverage:
	SMTP_PORT=1025 cargo tarpaulin --out Html

install-tarpaulin:
	cargo install cargo-tarpaulin
