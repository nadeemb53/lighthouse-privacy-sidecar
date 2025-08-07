#!/bin/bash

# 🚀 Simple Stealth Sidecar Demo - Foreground Execution
# 
# This demo runs entirely in foreground and responds to Ctrl+C
# Perfect for development and quick testing

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
NC='\033[0m' # No Color

# Ensure we're in the right directory
cd "$(dirname "$0")/.."

# 🧹 Clean up any existing demo processes first
echo -e "${PURPLE}🧹 Cleaning up any existing demo processes...${NC}"
pkill -f realistic-demo 2>/dev/null || true
pkill -f rainbow-attack 2>/dev/null || true
rm -f /tmp/stealth_demo_commands 2>/dev/null || true
sleep 1

echo
echo -e "${PURPLE}🚀 Reth-Stealth-Sidecar Simple Demo${NC}"
echo -e "${PURPLE}====================================${NC}"
echo
echo -e "${BLUE}This demo will:${NC}"
echo -e "${BLUE}  1. Start the realistic demo (foreground)${NC}"
echo -e "${BLUE}  2. Show stealth components working${NC}"
echo -e "${BLUE}  3. Connect to real Ethereum mainnet${NC}"
echo -e "${BLUE}  4. Exit cleanly with Ctrl+C${NC}"
echo
echo -e "${YELLOW}⚠️  Press Ctrl+C anytime to stop${NC}"
echo

# Determine duration
DURATION=${1:-60}  # Default 60 seconds, or use first argument
echo -e "${GREEN}🕐 Running for $DURATION seconds...${NC}"
echo

# Set up Ctrl+C handler
trap 'echo -e "\n${YELLOW}🛑 Demo stopped by user${NC}"; exit 0' INT

# Run the demo in foreground - this is the main process
echo -e "${PURPLE}🌐 Starting realistic demo (connecting to mainnet)...${NC}"
./target/release/realistic-demo \
    --duration $DURATION \
    --command-pipe /tmp/stealth_demo_commands

echo
echo -e "${GREEN}🎯 Demo completed successfully!${NC}"