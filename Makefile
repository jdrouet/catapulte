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
