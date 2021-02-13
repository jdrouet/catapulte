#!/bin/bash

set -xe

cmd_status="curl $1/status"
cmd_create_json="curl -X POST -v -H \"Content-Type: application/json\"  --data \"{\\\"from\\\":\\\"alice@example.com\\\",\\\"to\\\":\\\"bob@example.com\\\",\\\"params\\\":{}}\" $1/templates/user-login"

~/.cargo/bin/hyperfine --export-json hyperfine.json "$cmd_status" "$cmd_create_json"
