#!/usr/bin/env bash
set -e

echo "Container starting with ROLE=${ROLE}"

if [ "$ROLE" = "faucet" ]; then
  echo "Starting aptos-faucet-service with key: ${MINT_KEY}"
  echo "Node URL: ${NODE_URL}, Chain ID: ${CHAIN_ID}"
  exec /usr/local/bin/aptos-faucet-service run-simple \
       --key "$(cat "${MINT_KEY}")" \
       --node-url "${NODE_URL}" \
       --chain-id "${CHAIN_ID}"
elif [ "$ROLE" = "validator" ]; then
  echo "Starting aptos-node with config: ${CONFIG_PATH}"
  exec /usr/local/bin/aptos-node -f "${CONFIG_PATH}"
elif [ "$ROLE" = "full_node" ]; then
  echo "Starting aptos-node with config: ${CONFIG_PATH}"
  exec /usr/local/bin/aptos-node -f "${CONFIG_PATH}"
elif [ "$ROLE" = "healthcheck" ]; then
  echo "Starting aptos-healthcheck"
  sleep 10
  chmod +x /usr/local/bin/healthcheck.sh
  exec /usr/local/bin/healthcheck.sh "$NODE_COUNT" "$NETWORK_IP"
else
  echo "ERROR: Unknown ROLE: ${ROLE}"
  exit 1
fi
