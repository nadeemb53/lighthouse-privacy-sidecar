#!/bin/bash

# Demo script for reth-stealth-sidecar presentation
# This script demonstrates the RAINBOW attack vulnerability and the stealth sidecar defense

set -e

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$DEMO_DIR"

echo "🌈 reth-stealth-sidecar Demo Script"
echo "=================================="
echo "This demo shows how the stealth sidecar protects Ethereum validators"
echo "from the RAINBOW deanonymization attack."
echo ""

# Function to wait for services to be ready
wait_for_service() {
    local service=$1
    local port=$2
    local max_attempts=30
    local attempt=1

    echo "Waiting for $service to be ready on port $port..."
    while ! nc -z localhost $port && [ $attempt -le $max_attempts ]; do
        echo "  Attempt $attempt/$max_attempts..."
        sleep 2
        ((attempt++))
    done

    if [ $attempt -gt $max_attempts ]; then
        echo "❌ $service failed to start on port $port"
        exit 1
    else
        echo "✅ $service is ready"
    fi
}

# Step 1: Start the infrastructure
echo "Step 1: Starting Ethereum infrastructure..."
echo "==========================================="
docker-compose up -d reth lighthouse prometheus grafana

echo "Waiting for services to initialize..."
wait_for_service "Reth" 8545
wait_for_service "Lighthouse" 5052
wait_for_service "Prometheus" 9090
wait_for_service "Grafana" 3000

echo "✅ Core infrastructure is running"
echo ""

# Step 2: Start nwaku nodes for friend mesh
echo "Step 2: Starting Waku friend mesh..."
echo "====================================="
docker-compose up -d nwaku-1 nwaku-2 nwaku-3

echo "Waiting for Waku nodes..."
wait_for_service "nwaku-1" 8645
wait_for_service "nwaku-2" 8646
wait_for_service "nwaku-3" 8647

echo "✅ Friend mesh is ready"
echo ""

# Step 3: Demonstrate the vulnerability (baseline)
echo "Step 3: Demonstrating RAINBOW attack (baseline)..."
echo "=================================================="
echo "Running RAINBOW attack for 120 seconds without protection..."

# Run the attack tool
docker-compose run --rm rainbow-attacker \
    rainbow-attack-tool \
    --duration 120 \
    --confidence 0.8 \
    --output /results/baseline-attack.json

echo ""
echo "Baseline attack completed. Results:"
if [ -f "results/baseline-attack.json" ]; then
    cat results/baseline-attack.json | jq -r '
        "Validators mapped: \(.validators_mapped | length)",
        "Success rate: \(.success_rate * 100 | round)%",
        "Total observations: \(.total_attestations_observed)"
    '
else
    echo "No results file found - attack may have failed"
fi
echo ""

# Step 4: Start stealth sidecars
echo "Step 4: Starting stealth sidecars..."
echo "===================================="
docker-compose up -d stealth-sidecar-1 stealth-sidecar-2 stealth-sidecar-3

echo "Waiting for stealth sidecars..."
wait_for_service "stealth-sidecar-1" 9091
wait_for_service "stealth-sidecar-2" 9092
wait_for_service "stealth-sidecar-3" 9093

echo "✅ Stealth sidecars are active and protecting validators"
echo ""

# Give some time for subnet shuffling to take effect
echo "Waiting 30 seconds for subnet shuffling to take effect..."
sleep 30

# Step 5: Demonstrate the defense
echo "Step 5: Demonstrating stealth sidecar defense..."
echo "================================================"
echo "Running RAINBOW attack for 120 seconds WITH protection..."

# Run the attack again with protection active
docker-compose run --rm rainbow-attacker \
    rainbow-attack-tool \
    --duration 120 \
    --confidence 0.8 \
    --output /results/protected-attack.json

echo ""
echo "Protected attack completed. Results:"
if [ -f "results/protected-attack.json" ]; then
    cat results/protected-attack.json | jq -r '
        "Validators mapped: \(.validators_mapped | length)",
        "Success rate: \(.success_rate * 100 | round)%",
        "Total observations: \(.total_attestations_observed)"
    '
else
    echo "No results file found - attack may have failed"
fi
echo ""

# Step 6: Show metrics and results
echo "Step 6: Displaying metrics and performance..."
echo "============================================="

echo "Current stealth sidecar metrics:"
echo ""

# Check metrics from each sidecar
for i in 1 2 3; do
    port=$((9090 + i))
    echo "Stealth Sidecar $i metrics:"
    curl -s "http://localhost:$port/metrics" | grep -E "(stealth_sidecar|uptime)" | head -5 || echo "  Metrics not available"
    echo ""
done

echo "Grafana dashboard available at: http://localhost:3000"
echo "  Username: admin"
echo "  Password: stealth"
echo ""

echo "Prometheus available at: http://localhost:9090"
echo ""

# Step 7: Compare results
echo "Step 7: Comparing attack results..."
echo "==================================="

if [ -f "results/baseline-attack.json" ] && [ -f "results/protected-attack.json" ]; then
    echo "COMPARISON RESULTS:"
    echo ""
    
    baseline_success=$(cat results/baseline-attack.json | jq -r '.success_rate * 100 | round')
    protected_success=$(cat results/protected-attack.json | jq -r '.success_rate * 100 | round')
    
    baseline_mapped=$(cat results/baseline-attack.json | jq -r '.validators_mapped | length')
    protected_mapped=$(cat results/protected-attack.json | jq -r '.validators_mapped | length')
    
    echo "                    | Baseline | Protected | Improvement"
    echo "--------------------+----------+-----------+------------"
    echo "Success Rate        | ${baseline_success}%      | ${protected_success}%        | $((baseline_success - protected_success))% reduction"
    echo "Validators Mapped   | $baseline_mapped        | $protected_mapped         | $((baseline_mapped - protected_mapped)) fewer"
    echo ""
    
    if [ $protected_success -lt $baseline_success ]; then
        echo "🎉 SUCCESS! The stealth sidecar reduced the attack success rate!"
        echo ""
        echo "Privacy improvement: $((baseline_success - protected_success))% fewer validators mapped"
    else
        echo "⚠️  Defense needs tuning - success rates are similar"
    fi
else
    echo "❌ Could not compare results - missing result files"
fi

echo ""
echo "Demo completed!"
echo ""
echo "Key services running:"
echo "  Grafana:           http://localhost:3000 (admin/stealth)"
echo "  Prometheus:        http://localhost:9090"
echo "  Reth JSON-RPC:     http://localhost:8545"
echo "  Lighthouse API:    http://localhost:5052"
echo ""
echo "To stop all services: docker-compose down"
echo "To view logs: docker-compose logs [service-name]"
echo ""
echo "Results files:"
echo "  Baseline attack:   ./results/baseline-attack.json"
echo "  Protected attack:  ./results/protected-attack.json"