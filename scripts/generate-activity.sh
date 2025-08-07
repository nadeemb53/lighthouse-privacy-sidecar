#!/bin/bash

# Activity Generator for Live Demo
# Simulates attestation activity to make the demo more engaging

COMMAND_PIPE="/tmp/stealth_live_demo"

# Wait for command pipe to exist
echo -e "${YELLOW}⏳ Waiting for sidecar to start...${NC}"
while [ ! -p "$COMMAND_PIPE" ]; do
    sleep 1
done
DURATION=180

# Colors
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${CYAN}🎬 Activity Generator Started${NC}"

# Wait for sidecar to be ready
sleep 10

# Generate realistic attestation activity
end_time=$((SECONDS + DURATION))
while [ $SECONDS -lt $end_time ]; do
    # Random validator and subnet
    validator_id=$((RANDOM % 100))
    subnet_id=$((RANDOM % 64))
    
    # Simulate attestation data
    data="0x$(openssl rand -hex 32)"
    
    # Send attestation
    echo "send_attestation $validator_id $subnet_id $data" > "$COMMAND_PIPE" 2>/dev/null || true
    
    # Random delay between attestations (1-5 seconds)
    sleep $((1 + RANDOM % 5))
done

echo -e "${GREEN}✅ Activity generation completed${NC}"