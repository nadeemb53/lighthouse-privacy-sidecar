#!/bin/bash
# Simple utility to kill any running sidecar processes
pkill -f lighthouse-privacy-sidecar || true
echo "✅ All sidecar processes terminated"