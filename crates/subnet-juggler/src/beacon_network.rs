use anyhow::Result;
use futures::StreamExt;
use libp2p::{
    gossipsub::{self, IdentTopic, MessageId},
    identify,
    noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p_identity as identity;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use stealth_common::{EpochInfo, StealthError, StealthResult, SubnetId};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::NetworkingProvider;

/// Network behaviour for beacon chain gossipsub
#[derive(NetworkBehaviour)]
pub struct BeaconNetworkBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub identify: identify::Behaviour,
}

/// Command sent to the beacon network manager
#[derive(Debug)]
pub enum NetworkCommand {
    Subscribe {
        subnet_id: SubnetId,
        response: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        subnet_id: SubnetId,
        response: oneshot::Sender<Result<()>>,
    },
    GetPeerCount {
        response: oneshot::Sender<usize>,
    },
    GetEpochInfo {
        response: oneshot::Sender<StealthResult<EpochInfo>>,
    },
    Shutdown,
}

/// Events from the beacon network
#[derive(Debug)]
pub enum NetworkEvent {
    AttestationReceived {
        subnet_id: u8,
        peer_id: PeerId,
        message_data: Vec<u8>,
    },
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
}

/// Beacon chain network provider using real libp2p gossipsub
pub struct BeaconNetworkProvider {
    command_tx: mpsc::UnboundedSender<NetworkCommand>,
    event_rx: mpsc::UnboundedReceiver<NetworkEvent>,
    peer_count: usize,
}

impl BeaconNetworkProvider {
    /// Create a new beacon network provider
    pub async fn new(bootstrap_peers: Vec<String>) -> Result<Self> {
        info!("🌐 Initializing beacon chain libp2p network");
        
        // Create channels for communication
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        
        // Set up libp2p identity
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {local_peer_id}");

        // Configure gossipsub for Ethereum beacon chain
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Permissive)
            .message_id_fn(|message| {
                MessageId::from(&Sha256::digest(&message.data)[..20])
            })
            .build()
            .expect("Valid gossipsub config");

        // Create gossipsub behaviour
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        )
        .expect("Correct configuration");

        // Subscribe to backbone subnets (0 and 1 for demo)
        let fork_digest = "7a7b8b7f"; // Ethereum mainnet fork digest
        for subnet_id in 0..2 {
            let topic = IdentTopic::new(format!(
                "/eth2/{}/beacon_attestation_{}/ssz_snappy",
                fork_digest, subnet_id
            ));
            gossipsub.subscribe(&topic)?;
            info!("✅ Subscribed to attestation subnet {}", subnet_id);
        }

        // Create identify behaviour
        let identify = identify::Behaviour::new(
            identify::Config::new("/eth2/1.0.0".into(), local_key.public())
        );

        // Build network behaviour
        let behaviour = BeaconNetworkBehaviour {
            gossipsub,
            identify,
        };

        // Create swarm
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

        // Listen on local port
        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        // Connect to bootstrap peers
        for peer_addr in &bootstrap_peers {
            match peer_addr.parse::<Multiaddr>() {
                Ok(addr) => {
                    info!("🔗 Dialing bootstrap peer: {}", addr);
                    if let Err(e) = swarm.dial(addr) {
                        warn!("Failed to dial {}: {}", peer_addr, e);
                    }
                }
                Err(e) => {
                    warn!("Invalid bootstrap peer address {}: {}", peer_addr, e);
                }
            }
        }

        // Spawn the network event loop
        let command_tx_clone = command_tx.clone();
        tokio::spawn(async move {
            Self::run_network_loop(swarm, command_rx, event_tx, fork_digest.to_string()).await;
        });

        Ok(Self {
            command_tx: command_tx_clone,
            event_rx,
            peer_count: 0,
        })
    }

    /// Main network event loop
    async fn run_network_loop(
        mut swarm: libp2p::Swarm<BeaconNetworkBehaviour>,
        mut command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
        event_tx: mpsc::UnboundedSender<NetworkEvent>,
        fork_digest: String,
    ) {
        let mut subscribed_subnets = HashMap::new();

        loop {
            tokio::select! {
                // Handle swarm events
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(event) => match event {
                            BeaconNetworkBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                                propagation_source: peer_id,
                                message,
                                ..
                            }) => {
                                // Parse subnet from topic
                                if let Some(subnet_id) = Self::parse_subnet_from_topic(&message.topic.as_str()) {
                                    let _ = event_tx.send(NetworkEvent::AttestationReceived {
                                        subnet_id,
                                        peer_id,
                                        message_data: message.data,
                                    });
                                }
                            }
                            BeaconNetworkBehaviourEvent::Identify(identify::Event::Received { peer_id, info }) => {
                                debug!("🆔 Identified peer {}: {}", peer_id, info.protocol_version);
                            }
                            _ => {}
                        },
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!("👂 Listening on {address}");
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            info!("🤝 Connected to peer: {peer_id}");
                            let _ = event_tx.send(NetworkEvent::PeerConnected(peer_id));
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            debug!("👋 Disconnected from peer: {peer_id}");
                            let _ = event_tx.send(NetworkEvent::PeerDisconnected(peer_id));
                        }
                        _ => {}
                    }
                }
                
                // Handle commands
                Some(command) = command_rx.recv() => {
                    match command {
                        NetworkCommand::Subscribe { subnet_id, response } => {
                            let topic = IdentTopic::new(format!(
                                "/eth2/{}/beacon_attestation_{}/ssz_snappy",
                                fork_digest, subnet_id.0
                            ));
                            
                            let result = swarm.behaviour_mut().gossipsub.subscribe(&topic);
                            if result.is_ok() {
                                subscribed_subnets.insert(subnet_id.0, topic);
                                info!("✅ Subscribed to attestation subnet {}", subnet_id.0);
                            }
                            let _ = response.send(result.map(|_| ()).map_err(|e| anyhow::anyhow!("{}", e)));
                        }
                        NetworkCommand::Unsubscribe { subnet_id, response } => {
                            if let Some(topic) = subscribed_subnets.remove(&subnet_id.0) {
                                let result = swarm.behaviour_mut().gossipsub.unsubscribe(&topic);
                                if result.is_ok() {
                                    info!("✅ Unsubscribed from attestation subnet {}", subnet_id.0);
                                }
                                let _ = response.send(result.map(|_| ()).map_err(|e| anyhow::anyhow!("{}", e)));
                            } else {
                                let _ = response.send(Ok(()));
                            }
                        }
                        NetworkCommand::GetPeerCount { response } => {
                            let count = swarm.connected_peers().count();
                            let _ = response.send(count);
                        }
                        NetworkCommand::GetEpochInfo { response } => {
                            let epoch_info = Self::calculate_current_epoch();
                            let _ = response.send(epoch_info);
                        }
                        NetworkCommand::Shutdown => {
                            info!("📡 Shutting down beacon network");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Parse subnet ID from Ethereum beacon topic
    fn parse_subnet_from_topic(topic: &str) -> Option<u8> {
        // Format: /eth2/{fork_digest}/beacon_attestation_{subnet_id}/ssz_snappy
        if let Some(topic_suffix) = topic.strip_prefix("/eth2/") {
            let parts: Vec<&str> = topic_suffix.split('/').collect();
            if parts.len() >= 3 && parts[1].starts_with("beacon_attestation_") {
                return parts[1]
                    .strip_prefix("beacon_attestation_")?
                    .parse::<u8>()
                    .ok();
            }
        }
        None
    }

    /// Calculate current epoch based on system time
    fn calculate_current_epoch() -> StealthResult<EpochInfo> {
        const GENESIS_TIME: u64 = 1606824023; // Ethereum mainnet genesis
        const SECONDS_PER_SLOT: u64 = 12;
        const SLOTS_PER_EPOCH: u64 = 32;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| StealthError::Config(format!("Time error: {}", e)))?
            .as_secs();

        if now < GENESIS_TIME {
            return Err(StealthError::Config("Time before genesis".to_string()));
        }

        let elapsed = now - GENESIS_TIME;
        let current_slot = elapsed / SECONDS_PER_SLOT;
        let current_epoch = current_slot / SLOTS_PER_EPOCH;

        Ok(EpochInfo {
            epoch: current_epoch,
            slot: current_slot,
            slots_per_epoch: SLOTS_PER_EPOCH,
            seconds_per_slot: SECONDS_PER_SLOT,
        })
    }

    /// Get next network event
    pub async fn next_event(&mut self) -> Option<NetworkEvent> {
        self.event_rx.recv().await
    }

    /// Get current peer count
    pub async fn get_peer_count(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        if self.command_tx.send(NetworkCommand::GetPeerCount { response: tx }).is_ok() {
            rx.await.unwrap_or(0)
        } else {
            0
        }
    }
}

#[async_trait::async_trait]
impl NetworkingProvider for BeaconNetworkProvider {
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(NetworkCommand::Subscribe {
                subnet_id,
                response: tx,
            })
            .map_err(|_| StealthError::Network("Command channel closed".to_string()))?;

        rx.await
            .map_err(|_| StealthError::Network("Response channel closed".to_string()))?
            .map_err(|e| StealthError::Network(e.to_string()))
    }

    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(NetworkCommand::Unsubscribe {
                subnet_id,
                response: tx,
            })
            .map_err(|_| StealthError::Network("Command channel closed".to_string()))?;

        rx.await
            .map_err(|_| StealthError::Network("Response channel closed".to_string()))?
            .map_err(|e| StealthError::Network(e.to_string()))
    }

    async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(NetworkCommand::GetEpochInfo { response: tx })
            .map_err(|_| StealthError::Network("Command channel closed".to_string()))?;

        rx.await
            .map_err(|_| StealthError::Network("Response channel closed".to_string()))?
    }

    async fn get_validator_subnets(&self, _validator_pubkey: &str) -> StealthResult<Vec<SubnetId>> {
        // Return demo subnets - in reality would query Lighthouse
        Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
    }
}