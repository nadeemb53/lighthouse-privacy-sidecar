#!/bin/bash

# Stop nwaku node for reth-stealth-sidecar demo

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
NC='\033[0m'

NWAKU_DIR="../nwaku-compose"

echo -e "${BLUE}${WHITE}🔧 Stopping nwaku node${NC}"
echo ""

if [ ! -d "$NWAKU_DIR" ]; then
    echo -e "${YELLOW}⚠️  nwaku-compose directory not found${NC}"
    exit 0
fi

# Check if running
if ! curl -s http://localhost:8645/debug/v1/version >/dev/null 2>&1; then
    echo -e "${GREEN}✅ nwaku is not running${NC}"
    exit 0
fi

echo -e "${CYAN}🛑 Stopping nwaku containers...${NC}"
cd "$NWAKU_DIR"

# Stop and remove containers
docker compose down -v --remove-orphans

cd ../reth-stealth-sidecar

echo -e "${GREEN}✅ nwaku stopped${NC}"