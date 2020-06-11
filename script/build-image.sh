#!/bin/sh

TAG_NAME=$1

for platform in amd64 i386 arm64v8; do
  docker build --build-arg PLATFORM=${platform} --tag jdrouet/catapulte:${platform}-${TAG_NAME} .
done

for platform in amd64 i386 arm64v8; do
  docker push jdrouet/catapulte:${platform}-${TAG_NAME}
done

docker manifest create jdrouet/catapulte:${TAG_NAME} \
  jdrouet/catapulte:amd64-${TAG_NAME} \
  jdrouet/catapulte:i386-${TAG_NAME} \
  jdrouet/catapulte:arm64v8-${TAG_NAME}

docker manifest push jdrouet/catapulte:${TAG_NAME}
