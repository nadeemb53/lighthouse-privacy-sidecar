use anyhow::Result;
use libp2p::{
    gossipsub::{self, IdentTopic, MessageId},
    identify,
    swarm::{SwarmEvent, NetworkBehaviour},
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p_identity as identity;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::timeout;
use futures::StreamExt;
use tracing::{info, warn, error};

#[derive(NetworkBehaviour)]
struct TestBehaviour {
    gossipsub: gossipsub::Behaviour,
    identify: identify::Behaviour,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("libp2p_test=info,warn")
        .init();

    info!("🧪 Testing libp2p connectivity to Ethereum beacon nodes");
    
    // Create libp2p identity
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer ID: {}", local_peer_id);

    // Configure gossipsub
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(gossipsub::ValidationMode::Permissive)
        .message_id_fn(|message| {
            MessageId::from(&Sha256::digest(&message.data)[..20])
        })
        .build()
        .expect("Valid config");

    // Create gossipsub behaviour
    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub_config,
    ).expect("Correct configuration");

    // Subscribe to a single attestation subnet for testing
    let topic = IdentTopic::new("/eth2/7a7b8b7f/beacon_attestation_0/ssz_snappy");
    gossipsub.subscribe(&topic)?;
    info!("✅ Subscribed to test topic: {}", topic);

    // Create identify behaviour
    let identify = identify::Behaviour::new(
        identify::Config::new("/eth2/1.0.0".into(), local_key.public())
    );

    // Create behaviour
    let behaviour = TestBehaviour {
        gossipsub,
        identify,
    };

    // Build swarm
    let mut swarm = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            (libp2p::tls::Config::new, libp2p::noise::Config::new),
            yamux::Config::default,
        )?
        .with_behaviour(|_key| behaviour)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Listen on random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Test bootstrap peers
    let bootstrap_peers = vec![
        "/ip4/4.157.240.54/tcp/9000/p2p/16Uiu2HAm5a1z45GYvdBZgGh8b5jB6jm1YcgP5TdhqfqmpVsM6gFV",
        "/ip4/4.196.214.4/tcp/9000/p2p/16Uiu2HAm5CQgaLeFXLFpn7YbYfKXGTGgJBP1vKKg5gLJKPKe2VKb",
    ];

    info!("🔗 Attempting to connect to {} bootstrap peers...", bootstrap_peers.len());
    
    for peer_addr in &bootstrap_peers {
        match peer_addr.parse::<Multiaddr>() {
            Ok(addr) => {
                info!("🔗 Dialing: {}", addr);
                if let Err(e) = swarm.dial(addr) {
                    error!("❌ Failed to dial {}: {}", peer_addr, e);
                }
            }
            Err(e) => {
                error!("❌ Invalid address {}: {}", peer_addr, e);
            }
        }
    }

    let mut connected_peers: usize = 0;
    let mut messages_received: u64 = 0;
    
    // Run for 30 seconds to test connections
    let test_duration = Duration::from_secs(30);
    info!("⏱️  Running connectivity test for {} seconds...", test_duration.as_secs());
    
    if let Ok(_) = timeout(test_duration, async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(event) => {
                    match event {
                        TestBehaviourEvent::Gossipsub(gossipsub::Event::Message { 
                            propagation_source, message, .. 
                        }) => {
                    messages_received += 1;
                    info!("📨 RECEIVED MESSAGE #{}: {} bytes from peer {} on topic {}",
                        messages_received, message.data.len(), propagation_source, message.topic);
                }
                        TestBehaviourEvent::Identify(identify::Event::Received { 
                            peer_id, info 
                        }) => {
                            info!("🆔 Identified peer {}: agent={}, protocols={:?}", 
                                peer_id, info.agent_version, info.protocols);
                        }
                        _ => {}
                    }
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!("👂 Listening on: {}", address);
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    connected_peers += 1;
                    info!("✅ CONNECTED to peer: {} (total: {})", peer_id, connected_peers);
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    connected_peers = connected_peers.saturating_sub(1);
                    warn!("❌ Disconnected from peer: {} (remaining: {})", peer_id, connected_peers);
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    error!("❌ Outgoing connection failed to {:?}: {}", peer_id, error);
                }
                SwarmEvent::IncomingConnectionError { error, .. } => {
                    error!("❌ Incoming connection failed: {}", error);
                }
                _ => {}
            }
        }
    }).await {
        info!("⏱️  Test timeout reached");
    }

    info!("📊 CONNECTIVITY TEST RESULTS:");
    info!("   Connected peers: {}", connected_peers);
    info!("   Messages received: {}", messages_received);
    
    if connected_peers > 0 {
        info!("✅ SUCCESS: Connected to Ethereum beacon nodes!");
        if messages_received > 0 {
            info!("✅ SUCCESS: Received real gossipsub messages!");
        } else {
            warn!("⚠️  Connected but no messages received (may need more time)");
        }
    } else {
        error!("❌ FAILURE: Could not connect to any beacon nodes");
        info!("🔍 Possible issues:");
        info!("   - Bootstrap peers may be offline or unreachable");
        info!("   - Network configuration issues");
        info!("   - Firewall blocking connections");
        info!("   - Protocol version mismatch");
    }

    Ok(())
}