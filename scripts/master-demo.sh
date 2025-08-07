#!/bin/bash

# Master Demo Script for reth-stealth-sidecar
# Orchestrates the complete demo experience with multiple components

set -e

# Colors and formatting
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
NC='\033[0m'
BOLD='\033[1m'

# Configuration
DEMO_MODE="${1:-simple}"  # simple, enhanced, or full

# Clear screen and show header
clear
echo -e "${BLUE}${BOLD}"
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║                🎬 RETH-STEALTH-SIDECAR MASTER DEMO             ║"
echo "║               Choose Your Demo Experience Level                 ║"
echo "╚════════════════════════════════════════════════════════════════╝"
echo -e "${NC}"
echo ""

# Show demo options if no argument provided
if [ "$1" = "" ]; then
    echo -e "${WHITE}🎯 Available Demo Modes:${NC}"
    echo ""
    echo -e "${GREEN}1. Simple Demo (5 min)${NC}"
    echo -e "   ${CYAN}./scripts/master-demo.sh simple${NC}"
    echo -e "   📱 Perfect for: Hackathon pitches, quick presentations"
    echo -e "   🎬 What runs: Main live demo only"
    echo ""
    echo -e "${YELLOW}2. Enhanced Demo (10 min)${NC}"
    echo -e "   ${CYAN}./scripts/master-demo.sh enhanced${NC}"
    echo -e "   🎯 Perfect for: Technical deep-dives, judge demos"
    echo -e "   🎬 What runs: Live demo + real-time metrics dashboard"
    echo ""
    echo -e "${PURPLE}3. Full Demo (15 min)${NC}"
    echo -e "   ${CYAN}./scripts/master-demo.sh full${NC}"
    echo -e "   🚀 Perfect for: Conference presentations, investor demos"
    echo -e "   🎬 What runs: Live demo + metrics + activity generation"
    echo ""
    echo -e "${WHITE}💡 Recommendation: Start with 'simple' for most audiences${NC}"
    echo ""
    exit 0
fi

# Validate demo mode
case "$DEMO_MODE" in
    simple|enhanced|full)
        ;;
    *)
        echo -e "${RED}❌ Invalid demo mode: $DEMO_MODE${NC}"
        echo -e "${WHITE}Valid options: simple, enhanced, full${NC}"
        exit 1
        ;;
esac

echo -e "${WHITE}🎬 Starting ${DEMO_MODE} demo experience...${NC}"
echo ""

# Function to wait for user input
wait_for_user() {
    echo -e "${BLUE}Press any key to continue...${NC}"
    read -n 1 -s
    echo ""
}

# Function to open new terminal (macOS)
open_terminal_macos() {
    local script_name="$1"
    local title="$2"
    osascript <<EOF
tell application "Terminal"
    do script "cd $(pwd) && echo '🎬 $title' && echo '' && $script_name"
    set custom title of front window to "$title"
end tell
EOF
}

# Function to open new terminal (Linux)
open_terminal_linux() {
    local script_name="$1"
    local title="$2"
    if command -v gnome-terminal >/dev/null; then
        gnome-terminal --title="$title" -- bash -c "cd $(pwd) && echo '🎬 $title' && echo '' && $script_name; exec bash"
    elif command -v xterm >/dev/null; then
        xterm -title "$title" -e "cd $(pwd) && echo '🎬 $title' && echo '' && $script_name; exec bash" &
    else
        echo -e "${YELLOW}⚠️  Could not detect terminal. Please run manually: $script_name${NC}"
    fi
}

# Function to open new terminal (cross-platform)
open_terminal() {
    local script_name="$1"
    local title="$2"
    
    if [[ "$OSTYPE" == "darwin"* ]]; then
        open_terminal_macos "$script_name" "$title"
    else
        open_terminal_linux "$script_name" "$title"
    fi
}

# Build if needed
if [ ! -f "./target/release/reth-stealth-sidecar" ]; then
    echo -e "${YELLOW}🔧 Building reth-stealth-sidecar...${NC}"
    cargo build --release --quiet
    echo -e "${GREEN}✅ Build completed${NC}"
    echo ""
fi

# Demo execution based on mode
case "$DEMO_MODE" in
    "simple")
        echo -e "${GREEN}🎬 Simple Demo: Running main live demo${NC}"
        echo -e "${WHITE}   This will show the complete RAINBOW attack defense in a single terminal${NC}"
        echo ""
        wait_for_user
        
        # Just run the main demo
        ./scripts/live-demo.sh
        ;;
        
    "enhanced")
        echo -e "${YELLOW}🎬 Enhanced Demo: Live demo + real-time metrics${NC}"
        echo -e "${WHITE}   Terminal 1: Main demo presentation${NC}"
        echo -e "${WHITE}   Terminal 2: Live metrics dashboard${NC}"
        echo ""
        echo -e "${CYAN}📊 Starting metrics dashboard in new terminal...${NC}"
        
        # Start metrics dashboard in new terminal
        open_terminal "./scripts/metrics-dashboard.sh" "📊 Live Metrics Dashboard"
        
        echo -e "${GREEN}✅ Metrics dashboard started${NC}"
        echo ""
        echo -e "${WHITE}🎯 Ready to start main demo${NC}"
        wait_for_user
        
        # Run main demo
        ./scripts/live-demo.sh
        ;;
        
    "full")
        echo -e "${PURPLE}🎬 Full Demo: Complete production experience${NC}"
        echo -e "${WHITE}   Terminal 1: Main demo presentation${NC}"
        echo -e "${WHITE}   Terminal 2: Live metrics dashboard${NC}"
        echo -e "${WHITE}   Terminal 3: Network activity generator${NC}"
        echo ""
        
        echo -e "${CYAN}📊 Starting metrics dashboard...${NC}"
        open_terminal "./scripts/metrics-dashboard.sh" "📊 Live Metrics Dashboard"
        sleep 2
        
        echo -e "${CYAN}🎭 Starting activity generator...${NC}"
        open_terminal "./scripts/generate-activity.sh" "🎭 Network Activity Generator"
        sleep 2
        
        echo -e "${GREEN}✅ All components started${NC}"
        echo ""
        echo -e "${WHITE}🎯 You should now see:${NC}"
        echo -e "${WHITE}   - This terminal: Main demo${NC}"
        echo -e "${WHITE}   - Terminal 2: Live streaming metrics${NC}"
        echo -e "${WHITE}   - Terminal 3: Generating network activity${NC}"
        echo ""
        echo -e "${YELLOW}💡 Arrange windows side-by-side for maximum impact!${NC}"
        echo ""
        wait_for_user
        
        # Run main demo
        ./scripts/live-demo.sh
        ;;
esac

echo ""
echo -e "${GREEN}🎉 Demo completed successfully!${NC}"
echo ""

# Show next steps
case "$DEMO_MODE" in
    "simple")
        echo -e "${WHITE}🚀 Want more? Try:${NC}"
        echo -e "${CYAN}   ./scripts/master-demo.sh enhanced${NC} - Add live metrics"
        echo -e "${CYAN}   ./scripts/master-demo.sh full${NC} - Full production experience"
        ;;
    "enhanced")
        echo -e "${WHITE}🚀 Want the complete experience?${NC}"
        echo -e "${CYAN}   ./scripts/master-demo.sh full${NC} - Add network activity simulation"
        ;;
    "full")
        echo -e "${WHITE}🏆 You've experienced the complete demo!${NC}"
        echo -e "${CYAN}   Perfect for hackathon presentations and technical reviews${NC}"
        ;;
esac

echo ""
echo -e "${BLUE}📚 Learn more:${NC}"
echo -e "${WHITE}   📖 Full documentation: README.md${NC}"
echo -e "${WHITE}   🎬 Demo guide: DEMO.md${NC}"
echo -e "${WHITE}   🛠️  Build & test: ./scripts/quick-test.sh${NC}"
echo ""