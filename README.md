# Lighthouse Privacy Sidecar

**Framework for Ethereum validator privacy research with real networking components**

## What This Is

A working framework to defend Ethereum validators against RAINBOW deanonymization attacks. The [RAINBOW attack](https://arxiv.org/abs/2409.04366) demonstrates practical deanonymization of Ethereum validators, locating more than 15% of validators on the network using network timing patterns.

**Key Components:**
- ✅ Real libp2p networking connecting to Ethereum nodes
- ✅ Working SubnetJuggler with dynamic subnet management  
- ✅ FriendRelay framework for k-anonymity research
- ✅ RAINBOW attack simulation with measurable results
- ✅ Buildable Lighthouse integration patch

**Note:** Privacy benefits are theoretical - this is research/framework code for testing validator privacy concepts.

## Architecture

```
Validator → Privacy Sidecar → Ethereum Network
            ├── SubnetJuggler: Dynamic subnet shuffling
            └── FriendRelay: Friend mesh coordination
```

## Demo

Run the complete working demo:

```bash
./scripts/working-demo.sh
```

**What you'll see:**
- Real reth connection
- Real libp2p peer IDs and bootstrap
- Framework components starting and coordinating
- RAINBOW attack simulation (results are simulated)
- Live metrics at http://localhost:9090

## What Actually Works

**Real Components:**
- libp2p gossipsub networking (attempts real Ethereum beacon connections)
- reth HTTP integration with version verification
- SubnetJuggler and FriendRelay framework coordination
- Prometheus metrics collection
- Lighthouse validator integration patch (buildable, untested)

**Simulated/Theoretical:**
- RAINBOW attack effectiveness (simulation shows no privacy improvement)
- Attestation data when bootstrap connections timeout
- Privacy metrics and timing improvements
- Production privacy validation

## Project Structure

```
├── src/main.rs                    # Main sidecar
├── src/bin/realistic-demo.rs      # Demo with real networking
├── crates/subnet-juggler/         # Real libp2p subnet management
├── crates/friend-relay/           # Friend mesh framework
├── crates/metrics/               # Prometheus metrics
├── lighthouse-patch/             # Validator integration
└── scripts/working-demo.sh       # Working demo
```

## Development Status

This is **research code** suitable for:
- ✅ Privacy technique experimentation
- ✅ Framework development for validator privacy
- ✅ Foundation for production privacy solutions

**Not suitable for:**
- ❌ Production validator deployments
- ❌ Proven privacy protection claims

## Configuration

Customize via `config/stealth-sidecar.toml`:

```toml
extra_subnets_per_epoch = 8        # Subnet shuffling intensity
[network]
bootstrap_peers = [
    "/ip4/4.157.240.54/tcp/9000/p2p/16Uiu2HAm5a1z45GYvdBZgGh8b5jB6jm1YcgP5TdhqfqmpVsM6gFV"
]
```

## Next Steps

1. Validate privacy claims with real attack methodology
2. Complete Waku integration for production friend mesh
3. Fix bootstrap peer connectivity for live network data
4. Security audit of privacy assumptions

---

This provides a **working technical foundation** for validator privacy research. Components work and integrate properly, but privacy benefits need validation with real attacks.