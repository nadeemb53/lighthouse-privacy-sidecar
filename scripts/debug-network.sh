#!/bin/bash

# 🔍 Network Debugging Script for Stealth Sidecar
# This script helps diagnose libp2p connection issues

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
NC='\033[0m' # No Color

cd "$(dirname "$0")/.."

echo -e "${PURPLE}🔍 Network Debugging for Stealth Sidecar${NC}"
echo -e "${PURPLE}=======================================${NC}"
echo

# 1. Check if we can reach bootstrap peers
echo -e "${BLUE}1. Testing bootstrap peer connectivity...${NC}"
BOOTSTRAP_PEERS=(
    "4.157.240.54:9000"
    "4.196.214.4:9000" 
    "18.223.219.100:9000"
    "18.223.219.100:9001"
)

for peer in "${BOOTSTRAP_PEERS[@]}"; do
    echo -n "   Testing $peer... "
    if timeout 5 nc -z ${peer/:/ } 2>/dev/null; then
        echo -e "${GREEN}✅ Reachable${NC}"
    else
        echo -e "${RED}❌ Unreachable${NC}"
    fi
done

echo

# 2. Check if our ports are available
echo -e "${BLUE}2. Checking port availability...${NC}"
echo -n "   Port 9090 (metrics): "
if lsof -i :9090 >/dev/null 2>&1; then
    echo -e "${RED}❌ In use${NC}"
    echo "     Current process: $(lsof -i :9090 | tail -1)"
else
    echo -e "${GREEN}✅ Available${NC}"
fi

echo

# 3. Test basic libp2p connection
echo -e "${BLUE}3. Testing basic libp2p functionality...${NC}"
echo -e "${YELLOW}   Starting demo with debug logging for 10 seconds...${NC}"
echo

# Set up trap to kill demo after 10 seconds
(sleep 10 && pkill -f realistic-demo) &

# Run demo with more verbose logging
RUST_LOG="realistic_demo=debug,libp2p=info" ./target/release/realistic-demo \
    --duration 15 \
    --command-pipe /tmp/debug_pipe 2>&1 | head -50

echo
echo -e "${BLUE}4. Summary:${NC}"
echo -e "${YELLOW}   If you saw:${NC}"
echo -e "${GREEN}   ✅ 'Connected to peer' messages → Network working${NC}"
echo -e "${GREEN}   ✅ 'Subscribed to /eth2/...' → Topics configured${NC}"
echo -e "${GREEN}   ✅ 'Received attestation' → Getting real data${NC}"
echo -e "${RED}   ❌ No peer connections → Bootstrap issue${NC}"
echo -e "${RED}   ❌ No topic subscriptions → Configuration issue${NC}"
echo -e "${RED}   ❌ No attestations → Network quiet or topic format wrong${NC}"
echo

echo -e "${PURPLE}🎯 Debug complete!${NC}"