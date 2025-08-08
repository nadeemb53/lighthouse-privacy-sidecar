#!/bin/bash

# Setup nwaku node for reth-stealth-sidecar demo
# Ensures RLN-enabled Waku network is available for friend relay

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
NC='\033[0m'

NWAKU_DIR="../nwaku-compose"
NWAKU_URL="https://github.com/waku-org/nwaku-compose"

echo -e "${BLUE}${WHITE}🔧 Setting up nwaku node for RLN-enabled friend relay${NC}"
echo ""

# Check if Docker is running
if ! docker info >/dev/null 2>&1; then
    echo -e "${RED}❌ Docker is not running. Please start Docker and try again.${NC}"
    exit 1
fi

# Check if nwaku-compose exists
if [ ! -d "$NWAKU_DIR" ]; then
    echo -e "${YELLOW}📥 Cloning nwaku-compose...${NC}"
    cd ..
    git clone "$NWAKU_URL" || {
        echo -e "${RED}❌ Failed to clone nwaku-compose${NC}"
        exit 1
    }
    cd reth-stealth-sidecar
    echo -e "${GREEN}✅ nwaku-compose cloned${NC}"
else
    echo -e "${GREEN}✅ nwaku-compose already exists${NC}"
fi

# Setup .env file for simplified demo
echo -e "${CYAN}🔧 Configuring nwaku for demo...${NC}"

if [ ! -f "$NWAKU_DIR/.env" ]; then
    # Create a demo-friendly .env file
    cat > "$NWAKU_DIR/.env" << EOF
# Demo configuration for reth-stealth-sidecar
# This is a simplified setup for hackathon demonstration

# Domain for SSL certificate (not needed for local demo)
DOMAIN=localhost

# Ethereum Sepolia RPC endpoint (using public endpoint for demo)
ETH_CLIENT_ADDRESS=https://ethereum-sepolia-rpc.publicnode.com

# PostgreSQL password
POSTGRES_PASSWORD=test123

# Grafana admin password  
GRAFANA_PASSWORD=stealth

# RLN membership password (demo only - use secure password in production)
RLN_RELAY_ETH_PRIVATE_KEY_PASSWORD=demo123
RLN_RELAY_CRED_PASSWORD=demo123

# Waku node configuration
NODEKEY=0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef

# Storage configuration
STORAGE_SIZE=100MB

# For demo: reduced resource requirements
EXTRA_ARGS=--max-connections:50 --store-message-retention-policy:time:3600
EOF
    echo -e "${GREEN}✅ Created demo .env configuration${NC}"
else
    echo -e "${GREEN}✅ .env file already exists${NC}"
fi

# Check if already running
if curl -s http://localhost:8645/debug/v1/version >/dev/null 2>&1; then
    echo -e "${GREEN}✅ nwaku is already running${NC}"
    echo ""
    echo -e "${CYAN}📊 nwaku endpoints:${NC}"
    echo -e "   🌐 REST API: ${WHITE}http://localhost:8645${NC}"
    echo -e "   📈 Grafana: ${WHITE}http://localhost:3000${NC} (admin/stealth)"
    echo ""
    exit 0
fi

# Start nwaku
echo -e "${CYAN}🚀 Starting nwaku node...${NC}"
cd "$NWAKU_DIR"

# Start the services
echo -e "${YELLOW}⏳ Starting Docker containers...${NC}"
docker compose up -d

echo -e "${YELLOW}⏳ Waiting for nwaku API to be ready...${NC}"

# Wait for nwaku API to be available
for i in {1..30}; do
    if curl -s http://localhost:8645/debug/v1/version >/dev/null 2>&1; then
        echo -e "${GREEN}✅ nwaku is ready!${NC}"
        break
    fi
    if [ $i -eq 30 ]; then
        echo -e "${RED}❌ nwaku API not responding after 30 seconds${NC}"
        echo -e "${YELLOW}💡 Check logs: docker compose logs -f nwaku${NC}"
        cd ../reth-stealth-sidecar
        exit 1
    fi
    sleep 1
done

cd ../reth-stealth-sidecar

echo ""
echo -e "${GREEN}🎉 nwaku setup complete!${NC}"
echo ""
echo -e "${CYAN}📊 Available endpoints:${NC}"
echo -e "   🌐 REST API: ${WHITE}http://localhost:8645${NC}"
echo -e "   📈 Grafana: ${WHITE}http://localhost:3000${NC} (admin/stealth)"
echo ""
echo -e "${YELLOW}💡 The reth-stealth-sidecar can now use RLN-enabled friend relay!${NC}"

# Test the connection
echo -e "${CYAN}🧪 Testing nwaku connection...${NC}"
VERSION=$(curl -s http://localhost:8645/debug/v1/version 2>/dev/null || echo "unknown")
echo -e "${GREEN}✅ nwaku version: ${VERSION}${NC}"

echo ""
echo -e "${WHITE}🚀 Ready for demo with full RLN integration!${NC}"