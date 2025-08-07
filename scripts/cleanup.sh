#!/bin/bash

# 🧹 Clean up all demo processes and resources

echo "🧹 Cleaning up reth-stealth-sidecar demo processes..."

# Kill all demo-related processes
echo "Killing realistic-demo processes..."
pkill -f realistic-demo

echo "Killing rainbow-attack processes..."
pkill -f rainbow-attack

# Clean up command pipes
echo "Removing command pipes..."
rm -f /tmp/stealth_demo_commands

# Wait a moment for processes to exit
sleep 2

# Check if anything is still running
REMAINING=$(ps aux | grep -E "(realistic-demo|rainbow-attack)" | grep -v grep | wc -l)

if [ "$REMAINING" -eq 0 ]; then
    echo "✅ All demo processes cleaned up successfully"
else
    echo "⚠️  Still found $REMAINING running processes:"
    ps aux | grep -E "(realistic-demo|rainbow-attack)" | grep -v grep
    echo "You may need to kill them manually with: kill -9 [PID]"
fi

echo "🎯 Ready for a fresh demo start!"