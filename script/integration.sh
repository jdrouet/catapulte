#!/bin/bash

set -xe

curl $1/status

curl -X POST -v \
  -H "Content-Type: application/json" \
  --data "{\"from\":\"alice@example.com\",\"to\":\"bob@example.com\",\"params\":{}}" \
  $1/templates/user-login

