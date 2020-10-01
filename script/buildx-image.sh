#!/bin/sh

VERSION=$1

docker buildx build --push \
  --tag jdrouet/catapulte:$VERSION \
  --platform linux/amd64,linux/i386,linux/arm64,linux/arm/v7 \
  --label org.label-schema.version=$VERSION \
  --label org.label-schema.vcs-ref=$(git rev-parse --short HEAD) \
  .
