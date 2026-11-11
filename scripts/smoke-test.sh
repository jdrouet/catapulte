#!/bin/bash
set -euo pipefail

COMPOSE_FILE=${1:-}

if [ -z "$COMPOSE_FILE" ]; then
    echo "Usage: $0 <compose-file>"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OVERRIDE="$SCRIPT_DIR/../compose/smoke-test.override.yml"
PROJECT="catapulte-smoke"
NETWORK="${PROJECT}_default"
CURL_IMAGE="curlimages/curl:8.11.1"

COMPOSE=(docker compose -p "$PROJECT" -f "$COMPOSE_FILE" -f "$OVERRIDE")

# Reach catapulte and mailpit over the compose network rather than published
# host ports, so the suite never collides with whatever else is running.
run_curl() {
    docker run --rm --network "$NETWORK" "$CURL_IMAGE" "$@"
}

echo "Starting services with $COMPOSE_FILE..."
"${COMPOSE[@]}" up -d --build

# Cleanup on exit
trap '"${COMPOSE[@]}" down -v' EXIT

echo "Waiting for Catapulte to be ready..."
if ! run_curl --retry 30 --retry-delay 2 --retry-connrefused -sf \
    http://catapulte:3000/health/ready > /dev/null; then
    echo "Catapulte failed to become ready in time."
    "${COMPOSE[@]}" logs
    exit 1
fi

echo "Catapulte is ready. Sending test email..."
SUBMIT_RESPONSE=$(run_curl -s -X POST http://catapulte:3000/emails \
    -H "Content-Type: application/json" \
    -d '{
    "sender": "smoke-test@example.com",
    "recipients": [{"kind": "to", "address": "recipient@example.com"}],
    "subject": "Smoke Test",
    "body": {"kind": "plain", "text": "Hello from smoke test!"}
  }')

echo "Submit response: $SUBMIT_RESPONSE"
EMAIL_ID=$(echo "$SUBMIT_RESPONSE" | python3 -c "import sys, json; print(json.load(sys.stdin)['id'])")

if [ -z "$EMAIL_ID" ]; then
    echo "Failed to get email ID from submission response"
    exit 1
fi

echo "Email submitted with ID: $EMAIL_ID"

echo "Waiting for email to be delivered to Mailpit..."
# Wait a bit for processing
sleep 5

MAILPIT_RESPONSE=$(run_curl -s http://mailpit:8025/api/v1/messages)
FOUND=$(echo "$MAILPIT_RESPONSE" | python3 -c "
import sys, json
data = json.load(sys.stdin)
messages = data.get('messages', [])
found = any('smoke-test@example.com' in m.get('From', {}).get('Address', '') for m in messages)
print('true' if found else 'false')
")

if [ "$FOUND" == "true" ]; then
    echo "SUCCESS: Email found in Mailpit!"
else
    echo "FAILURE: Email not found in Mailpit."
    echo "Recent Mailpit messages: $MAILPIT_RESPONSE"
    "${COMPOSE[@]}" logs
    exit 1
fi

# An mjml_inline send goes through the render sandbox (a child process of the same binary).
echo "Sending an mjml_inline test email..."
MJML_RESPONSE=$(run_curl -s -X POST http://catapulte:3000/emails \
    -H "Content-Type: application/json" \
    -d '{
    "sender": "smoke-mjml@example.com",
    "recipients": [{"kind": "to", "address": "recipient@example.com"}],
    "subject": "Smoke MJML",
    "body": {"kind": "mjml_inline", "source": "<mjml><mj-body><mj-section><mj-column><mj-text>Hello {{ name }}</mj-text></mj-column></mj-section></mj-body></mjml>"},
    "variables": {"name": "smoke"}
  }')
echo "MJML submit response: $MJML_RESPONSE"
sleep 5

MJML_FOUND=$(run_curl -s http://mailpit:8025/api/v1/messages | python3 -c "
import sys, json
messages = json.load(sys.stdin).get('messages', [])
print('true' if any('smoke-mjml@example.com' in m.get('From', {}).get('Address', '') for m in messages) else 'false')
")

if [ "$MJML_FOUND" == "true" ]; then
    echo "SUCCESS: mjml_inline email found in Mailpit!"
else
    echo "FAILURE: mjml_inline email not found in Mailpit."
    "${COMPOSE[@]}" logs
    exit 1
fi

echo "Smoke test passed for $COMPOSE_FILE"
