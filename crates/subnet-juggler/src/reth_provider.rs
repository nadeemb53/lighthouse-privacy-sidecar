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

/// Network behaviour for the reth sidecar
#[derive(NetworkBehaviour)]
pub struct RethSidecarBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub identify: identify::Behaviour,
}

/// Command sent to the network manager
#[derive(Debug)]
pub enum NetworkCommand {
    Subscribe {
        subnet_id: SubnetId,
        response: oneshot::Sender<StealthResult<()>>,
    },
    Unsubscribe {
        subnet_id: SubnetId,
        response: oneshot::Sender<StealthResult<()>>,
    },
    GetPeerCount {
        response: oneshot::Sender<usize>,
    },
    Shutdown,
}

/// Events from the network manager
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    AttestationReceived {
        subnet_id: u8,
        data: Vec<u8>,
        source: PeerId,
    },
    SubnetSubscribed(SubnetId),
    SubnetUnsubscribed(SubnetId),
}

/// Real libp2p networking provider that manages its own swarm
pub struct RethNetworkProvider {
    command_tx: mpsc::UnboundedSender<NetworkCommand>,
    event_rx: mpsc::UnboundedReceiver<NetworkEvent>,
    genesis_time: SystemTime,
    current_subscriptions: HashMap<SubnetId, bool>,
}

impl RethNetworkProvider {
    /// Create a new RethNetworkProvider with real libp2p networking
    pub async fn new(bootstrap_peers: Vec<String>) -> Result<Self> {
        info!("🌐 Initializing reth sidecar libp2p network");

        // Create libp2p identity
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {local_peer_id}");

        // Configure gossipsub for Ethereum consensus layer
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_millis(700))
            .validation_mode(gossipsub::ValidationMode::Permissive) // Accept all for privacy monitoring
            .message_id_fn(|message| {
                MessageId::from(&Sha256::digest(&message.data)[..20])
            })
            .max_transmit_size(1048576) // 1MB max message size
            .duplicate_cache_time(Duration::from_secs(60))
            .build()
            .expect("Valid gossipsub config");

        // Create gossipsub behaviour
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        )
        .expect("Correct gossipsub configuration");

        // Create identify behaviour for peer discovery
        let identify = identify::Behaviour::new(
            identify::Config::new("/reth-stealth-sidecar/1.0.0".into(), local_key.public())
                .with_interval(Duration::from_secs(30)),
        );

        // Create network behaviour
        let behaviour = RethSidecarBehaviour { gossipsub, identify };

        // Build the swarm
        let mut swarm = SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().port_reuse(true),
                (libp2p::tls::Config::new, noise::Config::new),
                yamux::Config::default,
            )?
            .with_behaviour(|_key| behaviour)?
            .with_swarm_config(|c| {
                c.with_idle_connection_timeout(Duration::from_secs(60))
                    .with_max_negotiating_inbound_streams(128)
            })
            .build();

        // Listen on all interfaces with random port
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

        // Create channels for communication
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Spawn the network manager task
        tokio::spawn(Self::run_network_manager(swarm, command_rx, event_tx));

        let genesis_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1606824023);

        Ok(Self {
            command_tx,
            event_rx,
            genesis_time,
            current_subscriptions: HashMap::new(),
        })
    }

    /// Background task that manages the libp2p swarm
    async fn run_network_manager(
        mut swarm: libp2p::Swarm<RethSidecarBehaviour>,
        mut command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
        event_tx: mpsc::UnboundedSender<NetworkEvent>,
    ) {
        let mut subscribed_topics: HashMap<SubnetId, IdentTopic> = HashMap::new();

        loop {
            tokio::select! {
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(RethSidecarBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message {
                                propagation_source: peer_id,
                                message,
                                ..
                            }
                        )) => {
                            // Parse subnet ID from topic
                            if let Some(subnet_id) = Self::extract_subnet_id_from_topic(&message.topic.as_str()) {
                                debug!("📨 Received attestation on subnet {} from {}", subnet_id, peer_id);
                                
                                let _ = event_tx.send(NetworkEvent::AttestationReceived {
                                    subnet_id,
                                    data: message.data,
                                    source: peer_id,
                                });
                            }
                        }
                        SwarmEvent::Behaviour(RethSidecarBehaviourEvent::Identify(
                            identify::Event::Received { peer_id, info }
                        )) => {
                            debug!("🆔 Identified peer {}: {}", peer_id, info.protocol_version);
                        }
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

                command = command_rx.recv() => {
                    match command {
                        Some(NetworkCommand::Subscribe { subnet_id, response }) => {
                            let result = Self::handle_subscribe(&mut swarm, &mut subscribed_topics, subnet_id).await;
                            
                            if result.is_ok() {
                                let _ = event_tx.send(NetworkEvent::SubnetSubscribed(subnet_id));
                            }
                            
                            let _ = response.send(result);
                        }
                        Some(NetworkCommand::Unsubscribe { subnet_id, response }) => {
                            let result = Self::handle_unsubscribe(&mut swarm, &mut subscribed_topics, subnet_id).await;
                            
                            if result.is_ok() {
                                let _ = event_tx.send(NetworkEvent::SubnetUnsubscribed(subnet_id));
                            }
                            
                            let _ = response.send(result);
                        }
                        Some(NetworkCommand::GetPeerCount { response }) => {
                            let peer_count = swarm.network_info().num_peers();
                            let _ = response.send(peer_count);
                        }
                        Some(NetworkCommand::Shutdown) => {
                            info!("🛑 Shutting down network manager");
                            break;
                        }
                        None => {
                            warn!("Command channel closed, shutting down network manager");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle_subscribe(
        swarm: &mut libp2p::Swarm<RethSidecarBehaviour>,
        subscribed_topics: &mut HashMap<SubnetId, IdentTopic>,
        subnet_id: SubnetId,
    ) -> StealthResult<()> {
        // Current Ethereum mainnet fork digest (Electra)
        let fork_digest = "7a7b8b7f";
        let topic_string = format!(
            "/eth2/{}/beacon_attestation_{}/ssz_snappy",
            fork_digest, subnet_id.0
        );
        let topic = IdentTopic::new(topic_string);

        match swarm.behaviour_mut().gossipsub.subscribe(&topic) {
            Ok(_) => {
                subscribed_topics.insert(subnet_id, topic.clone());
                info!("✅ Subscribed to attestation subnet {}", subnet_id.0);
                Ok(())
            }
            Err(e) => {
                error!("❌ Failed to subscribe to subnet {}: {}", subnet_id.0, e);
                Err(StealthError::Network(format!(
                    "Failed to subscribe to subnet {}: {}",
                    subnet_id.0, e
                )))
            }
        }
    }

    async fn handle_unsubscribe(
        swarm: &mut libp2p::Swarm<RethSidecarBehaviour>,
        subscribed_topics: &mut HashMap<SubnetId, IdentTopic>,
        subnet_id: SubnetId,
    ) -> StealthResult<()> {
        if let Some(topic) = subscribed_topics.remove(&subnet_id) {
            match swarm.behaviour_mut().gossipsub.unsubscribe(&topic) {
                Ok(_) => {
                    info!("✅ Unsubscribed from attestation subnet {}", subnet_id.0);
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Failed to unsubscribe from subnet {}: {}", subnet_id.0, e);
                    Err(StealthError::Network(format!(
                        "Failed to unsubscribe from subnet {}: {}",
                        subnet_id.0, e
                    )))
                }
            }
        } else {
            warn!("Attempted to unsubscribe from subnet {} but not subscribed", subnet_id.0);
            Ok(())
        }
    }

    fn extract_subnet_id_from_topic(topic: &str) -> Option<u8> {
        // Topic format: /eth2/{fork_digest}/beacon_attestation_{subnet_id}/ssz_snappy
        if let Some(topic_suffix) = topic.strip_prefix("/eth2/") {
            let parts: Vec<&str> = topic_suffix.split('/').collect();
            if parts.len() >= 3 && parts[1].starts_with("beacon_attestation_") {
                parts[1]
                    .strip_prefix("beacon_attestation_")?
                    .parse::<u8>()
                    .ok()
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Get peer count from the network
    pub async fn get_peer_count(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        if self
            .command_tx
            .send(NetworkCommand::GetPeerCount { response: tx })
            .is_ok()
        {
            rx.await.unwrap_or(0)
        } else {
            0
        }
    }

    /// Receive the next network event (non-blocking)
    pub async fn next_event(&mut self) -> Option<NetworkEvent> {
        self.event_rx.recv().await
    }

    /// Check if we're subscribed to a subnet
    pub fn is_subscribed(&self, subnet_id: &SubnetId) -> bool {
        self.current_subscriptions.get(subnet_id).copied().unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl NetworkingProvider for RethNetworkProvider {
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(NetworkCommand::Subscribe {
                subnet_id,
                response: tx,
            })
            .map_err(|_| StealthError::Network("Network manager unavailable".to_string()))?;

        rx.await
            .map_err(|_| StealthError::Network("Response channel closed".to_string()))?
    }

    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(NetworkCommand::Unsubscribe {
                subnet_id,
                response: tx,
            })
            .map_err(|_| StealthError::Network("Network manager unavailable".to_string()))?;

        rx.await
            .map_err(|_| StealthError::Network("Response channel closed".to_string()))?
    }

    async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo> {
        // Calculate current epoch using system time and Ethereum constants
        const SECONDS_PER_SLOT: u64 = 12;
        const SLOTS_PER_EPOCH: u64 = 32;

        let elapsed = SystemTime::now()
            .duration_since(self.genesis_time)
            .map_err(|e| StealthError::Config(format!("Time error: {}", e)))?;

        let current_slot = elapsed.as_secs() / SECONDS_PER_SLOT;
        let current_epoch = current_slot / SLOTS_PER_EPOCH;

        Ok(EpochInfo {
            epoch: current_epoch,
            slot: current_slot,
            slots_per_epoch: SLOTS_PER_EPOCH,
            seconds_per_slot: SECONDS_PER_SLOT,
        })
    }

    async fn get_validator_subnets(&self, _validator_pubkey: &str) -> StealthResult<Vec<SubnetId>> {
        // For demo, return fixed backbone subnets
        // In reality, this would be calculated from validator index and epoch
        Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
    }
}

impl Drop for RethNetworkProvider {
    fn drop(&mut self) {
        let _ = self.command_tx.send(NetworkCommand::Shutdown);
    }
}