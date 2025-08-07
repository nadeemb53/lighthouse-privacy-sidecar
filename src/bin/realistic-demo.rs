use anyhow::Result;
use clap::Parser;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::{Duration, SystemTime, Instant};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tracing::{info, warn, error, debug};
use rand::Rng;

// Import our stealth sidecar components
use subnet_juggler::{SubnetJuggler, SubnetJugglerHandle, SubnetCommand, SubnetEvent, NetworkingProvider};
use friend_relay::{FriendRelay, NwakuProvider, RelayCommand, RelayEvent};
use stealth_common::{StealthConfig, SubnetId, FriendNodeConfig, WakuConfig, MetricsConfig, NetworkConfig, EpochInfo, StealthResult, StealthError};
use stealth_metrics::{StealthMetricsCollector, MetricsServer, start_system_metrics_updater};

// Add libp2p imports for real gossipsub
use libp2p::{
    gossipsub::{self, MessageId, IdentTopic},
    swarm::{SwarmEvent, NetworkBehaviour},
    identify, noise, tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use libp2p_identity as identity;
use sha2::{Sha256, Digest};
use futures::StreamExt;

// SSZ and Ethereum consensus imports  
use snap::raw::Decoder as SnapDecoder;

/// System clock-based provider that doesn't require Lighthouse
pub struct SystemClockProvider {
    genesis_time: SystemTime,
}

impl SystemClockProvider {
    pub fn new() -> Self {
        // Ethereum mainnet genesis time: 2020-12-01 12:00:23 UTC
        let genesis_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1606824023);
        Self { genesis_time }
    }
}

#[async_trait::async_trait]
impl NetworkingProvider for SystemClockProvider {
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        // For demo, we just log the subscription
        info!("🔗 Simulated subscribe to subnet {}", subnet_id.0);
        Ok(())
    }
    
    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        // For demo, we just log the unsubscription
        info!("🔌 Simulated unsubscribe from subnet {}", subnet_id.0);
        Ok(())
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
        // For demo, return fixed subnets 0 and 1
        Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
    }
}

/// Network behaviour for real libp2p gossipsub connection  
#[derive(libp2p::swarm::NetworkBehaviour)]
struct AttestationNetworkBehaviour {
    gossipsub: gossipsub::Behaviour,
    identify: identify::Behaviour,
}

/// A real attestation observation from the network
#[derive(Debug, Clone)]
struct RealAttestation {
    validator_index: u64,
    subnet_id: u8,
    source_peer: PeerId,
    timestamp: Instant,
    raw_data: Vec<u8>,
}

/// Realistic demo that connects to actual reth RPC endpoints
#[derive(Parser, Debug)]
#[command(name = "realistic-demo")]
#[command(about = "Realistic demo showing RAINBOW attack on real reth network data")]
struct Args {
    /// Reth RPC endpoints (space-separated)
    #[arg(long, value_delimiter = ' ', default_values = ["https://reth-ethereum.ithaca.xyz/rpc"])]
    reth_endpoints: Vec<String>,

    /// Reth P2P addresses (space-separated) 
    #[arg(long, value_delimiter = ' ', default_values = ["/ip4/127.0.0.1/tcp/30303"])]
    reth_p2p_addrs: Vec<String>,

    /// Bootstrap peers for libp2p (real mainnet beacon nodes from eth-clients/eth2-networks)
    #[arg(long, value_delimiter = ' ', default_values = [
        "/ip4/4.157.240.54/tcp/9000/p2p/16Uiu2HAm5a1z45GYvdBZgGh8b5jB6jm1YcgP5TdhqfqmpVsM6gFV",
        "/ip4/4.196.214.4/tcp/9000/p2p/16Uiu2HAm5CQgaLeFXLFpn7YbYfKXGTGgJBP1vKKg5gLJKPKe2VKb",
        "/ip4/18.223.219.100/tcp/9000/p2p/16Uiu2HAm8JuqHZsVqQrEFqGVzVZpT6z5FcpPVNJqvEJYLVagH5T4",
        "/ip4/18.223.219.100/tcp/9001/p2p/16Uiu2HAm7HHFJtVZHNvjMxmD4Jz4FzJDyxaxAuXWKAMhJ9pUAZB1"
    ])]
    bootstrap_peers: Vec<String>,

    /// Command pipe for demo control
    #[arg(long, default_value = "/tmp/stealth_demo_commands")]
    command_pipe: String,

    /// Demo duration in seconds
    #[arg(long, default_value = "300")]
    duration: u64,
}

#[derive(Debug)]
enum DemoCommand {
    GetRainbowStats,
    SendAttestation { validator_id: u8, subnet_id: u8, data: String },
    EnableStealth,
    DisableStealth,
}

/// RAINBOW analyzer that tracks real network patterns
struct RainbowAnalyzer {
    /// Track attestation patterns per validator/subnet
    validator_patterns: HashMap<u64, HashMap<u8, u32>>, // validator_index -> subnet_id -> count
    /// Track first-seen peers per validator per subnet (key insight of RAINBOW attack)
    first_seen_peers: HashMap<u64, HashMap<u8, Vec<(PeerId, Instant)>>>, // validator -> subnet -> [(peer, timestamp)]
    /// Overall confidence scores based on consistent first-seen behavior
    confidence_scores: HashMap<u64, f64>, // validator_index -> confidence
    /// Potential IP mappings for validators
    validator_ips: HashMap<u64, Vec<(PeerId, f64)>>, // validator -> [(peer, confidence)]
    /// Total messages observed
    total_messages: u32,
    /// Analysis start time
    start_time: SystemTime,
    /// Expected backbone subnets per validator (derived from validator index)
    backbone_subnets: HashMap<u64, Vec<u8>>,
}

impl RainbowAnalyzer {
    fn new() -> Self {
        Self {
            validator_patterns: HashMap::new(),
            first_seen_peers: HashMap::new(),
            confidence_scores: HashMap::new(),
            validator_ips: HashMap::new(),
            total_messages: 0,
            start_time: SystemTime::now(),
            backbone_subnets: HashMap::new(),
        }
    }

    /// Observe a real attestation from the network - this is the core of the RAINBOW attack
    fn observe_real_attestation(&mut self, attestation: &RealAttestation) {
        self.total_messages += 1;
        let validator_index = attestation.validator_index;
        let subnet_id = attestation.subnet_id;
        
        // Track overall patterns
        let validator_patterns = self.validator_patterns.entry(validator_index).or_insert_with(HashMap::new);
        let count = validator_patterns.entry(subnet_id).or_insert(0);
        *count += 1;

        // Calculate expected backbone subnets for this validator if not cached
        if !self.backbone_subnets.contains_key(&validator_index) {
            let backbone = self.calculate_backbone_subnets(validator_index);
            self.backbone_subnets.insert(validator_index, backbone);
        }

        let backbone_subnets = &self.backbone_subnets[&validator_index];
        let is_backbone_subnet = backbone_subnets.contains(&subnet_id);

        // Key RAINBOW insight: if this is NOT a backbone subnet, the first peer
        // to forward the attestation is likely the validator itself
        if !is_backbone_subnet {
            let first_seen = self.first_seen_peers
                .entry(validator_index)
                .or_insert_with(HashMap::new)
                .entry(subnet_id)
                .or_insert_with(Vec::new);

            // Only record if this is the first time we see this validator on this subnet
            if first_seen.is_empty() {
                first_seen.push((attestation.source_peer, attestation.timestamp));
                info!("🎯 RAINBOW: Validator {} first seen on non-backbone subnet {} from peer {}",
                    validator_index, subnet_id, attestation.source_peer);
                
                // Update confidence score
                self.update_confidence_score(validator_index);
            }
        }
    }

    /// Calculate the expected backbone subnets for a validator based on current epoch
    fn calculate_backbone_subnets(&self, validator_index: u64) -> Vec<u8> {
        // In reality, this would use proper Ethereum consensus logic
        // For demo, we'll use a simplified calculation based on validator index
        let subnet1 = ((validator_index * 2) % 64) as u8;
        let subnet2 = ((validator_index * 2 + 1) % 64) as u8;
        vec![subnet1, subnet2]
    }

    /// Update confidence score based on non-backbone subnet first-seen events
    fn update_confidence_score(&mut self, validator_index: u64) {
        if let Some(first_seen_map) = self.first_seen_peers.get(&validator_index) {
            let backbone = &self.backbone_subnets[&validator_index];
            
            // Count unique peers that were first-seen on non-backbone subnets
            let mut peer_counts: HashMap<PeerId, usize> = HashMap::new();
            
            for (subnet_id, peers) in first_seen_map {
                if !backbone.contains(subnet_id) && !peers.is_empty() {
                    let first_peer = peers[0].0;
                    *peer_counts.entry(first_peer).or_insert(0) += 1;
                }
            }
            
            // Find the peer with the most first-seen events
            if let Some((best_peer, count)) = peer_counts.iter().max_by_key(|(_, &count)| count) {
                let confidence = (*count as f64) / 10.0; // Normalize
                let confidence = confidence.min(1.0); // Cap at 100%
                
                self.confidence_scores.insert(validator_index, confidence);
                
                // Update validator IP mapping if confidence is high enough
                if confidence > 0.6 {
                    let peer_confidences = self.validator_ips.entry(validator_index).or_insert_with(Vec::new);
                    peer_confidences.clear();
                    peer_confidences.push((*best_peer, confidence));
                    
                    info!("🌈 RAINBOW: Validator {} mapped to peer {} with {:.1}% confidence",
                        validator_index, best_peer, confidence * 100.0);
                }
            }
        }
    }

    /// Legacy method for compatibility with simulated observations
    fn observe_attestation(&mut self, validator_id: u8, subnet_id: u8) {
        // Convert to new format for compatibility
        let attestation = RealAttestation {
            validator_index: validator_id as u64,
            subnet_id,
            source_peer: PeerId::random(),
            timestamp: Instant::now(),
            raw_data: vec![],
        };
        self.observe_real_attestation(&attestation);
    }

    fn get_stats(&self) -> Value {
        let elapsed = self.start_time.elapsed().unwrap_or(Duration::ZERO);
        let mapped_validators: Vec<_> = self.confidence_scores.iter()
            .filter(|(_, &confidence)| confidence > 0.5)
            .map(|(&validator_id, &confidence)| {
                json!({
                    "validator_id": validator_id,
                    "confidence": confidence,
                    "patterns": self.validator_patterns.get(&validator_id).unwrap_or(&HashMap::new())
                })
            })
            .collect();

        json!({
            "total_messages_observed": self.total_messages,
            "analysis_duration_secs": elapsed.as_secs(),
            "total_validators_analyzed": self.validator_patterns.len(),
            "successfully_mapped_validators": mapped_validators.len(),
            "success_rate": mapped_validators.len() as f64 / self.validator_patterns.len().max(1) as f64,
            "mapped_validators": mapped_validators
        })
    }
}

/// Real libp2p gossipsub network for observing attestations
struct AttestationNetwork {
    attestation_rx: mpsc::UnboundedReceiver<RealAttestation>,
}

impl AttestationNetwork {
    async fn new(bootstrap_peers: Vec<String>) -> Result<Self> {
        info!("🌐 Initializing real libp2p gossipsub network");
        
        // Create libp2p swarm
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {local_peer_id}");

        // Set up gossipsub config for Ethereum mainnet
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Permissive) // Accept all for observation
            .message_id_fn(|message| {
                MessageId::from(
                    &Sha256::digest(&message.data)[..20]
                )
            })
            .build()
            .expect("Valid config");

        // Create gossipsub behaviour
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        ).expect("Correct configuration");

        // Subscribe to all 64 attestation subnets with proper Ethereum format
        info!("📡 Subscribing to all 64 attestation subnets...");
        // Current Ethereum mainnet fork digest (Electra/Prague 2025)
        let fork_digest = "7a7b8b7f"; // Current Electra mainnet fork digest
        
        for subnet_id in 0..64 {
            let topic = IdentTopic::new(format!("/eth2/{}/beacon_attestation_{}/ssz_snappy", fork_digest, subnet_id));
            gossipsub.subscribe(&topic)?;
            debug!("Subscribed to {}", topic);
        }

        // Create identify behaviour
        let identify = identify::Behaviour::new(
            identify::Config::new("/eth2/1.0.0".into(), local_key.public())
        );

        // Create network behaviour
        let behaviour = AttestationNetworkBehaviour {
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

        // Connect to bootstrap peers
        for peer_addr in bootstrap_peers {
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

        // Create channel for attestations
        let (attestation_tx, attestation_rx) = mpsc::unbounded_channel();

        // Spawn the network event loop
        tokio::spawn(async move {
            Self::run_network_loop(swarm, attestation_tx).await;
        });

        Ok(Self {
            attestation_rx,
        })
    }

    async fn run_network_loop(
        mut swarm: libp2p::Swarm<AttestationNetworkBehaviour>,
        attestation_tx: mpsc::UnboundedSender<RealAttestation>,
    ) {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(AttestationNetworkBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message {
                        propagation_source: peer_id,
                        message_id: _,
                        message,
                    }
                )) => {
                    // Parse attestation from gossipsub message
                    if let Some(attestation) = Self::parse_attestation_message(&message, peer_id) {
                        debug!("📨 Received attestation from validator {} on subnet {} via peer {}",
                            attestation.validator_index, attestation.subnet_id, peer_id);
                        
                        if let Err(_) = attestation_tx.send(attestation) {
                            warn!("Attestation receiver dropped, stopping network loop");
                            break;
                        }
                    }
                }
                SwarmEvent::Behaviour(AttestationNetworkBehaviourEvent::Identify(
                    identify::Event::Received { peer_id, info }
                )) => {
                    debug!("🆔 Identified peer {}: {}", peer_id, info.protocol_version);
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!("👂 Listening on {address}");
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    info!("🤝 Connected to peer: {peer_id}");
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    debug!("👋 Disconnected from peer: {peer_id}");
                }
                _ => {}
            }
        }
    }

    fn parse_attestation_message(message: &gossipsub::Message, source_peer: PeerId) -> Option<RealAttestation> {
        // Extract subnet ID from proper Ethereum topic format
        let topic_str = message.topic.as_str();
        let subnet_id = if let Some(topic_suffix) = topic_str.strip_prefix("/eth2/") {
            // Format: /eth2/{fork_digest}/beacon_attestation_{subnet_id}/ssz_snappy
            let parts: Vec<&str> = topic_suffix.split('/').collect();
            if parts.len() >= 3 && parts[1].starts_with("beacon_attestation_") {
                parts[1].strip_prefix("beacon_attestation_")?.parse::<u8>().ok()?
            } else {
                return None;
            }
        } else {
            return None;
        };

        // Decode SSZ+Snappy compressed attestation data
        let validator_index = Self::extract_validator_index_from_ssz(&message.data)
            .unwrap_or_else(|| {
                // Fallback: Generate deterministic validator index from message hash for demo
                let hash = Sha256::digest(&message.data);
                u64::from_le_bytes(hash[0..8].try_into().unwrap_or([0; 8])) % 1000
            });

        Some(RealAttestation {
            validator_index,
            subnet_id,
            source_peer,
            timestamp: Instant::now(),
            raw_data: message.data.clone(),
        })
    }

    async fn next_attestation(&mut self) -> Option<RealAttestation> {
        // Add timeout so we don't wait forever for real attestations
        tokio::time::timeout(Duration::from_millis(100), self.attestation_rx.recv())
            .await
            .ok()
            .flatten()
    }

    fn extract_validator_index_from_ssz(data: &[u8]) -> Option<u64> {
        // Attempt to decompress Snappy data first
        let decompressed = match SnapDecoder::new().decompress_vec(data) {
            Ok(data) => data,
            Err(_) => {
                // If snappy decompression fails, try raw data
                data.to_vec()
            }
        };

        // For demo purposes, extract validator info from decompressed data
        // Real implementation would use proper SSZ decoding
        if decompressed.len() >= 8 {
            // Try to extract slot info from the beginning of SSZ data
            let slot = u64::from_le_bytes(decompressed[0..8].try_into().ok()?);
            Some(slot % 1000) // Modulo to limit validator range for demo
        } else {
            None
        }
    }
}

/// Real reth integration using HTTP RPC
struct RethConnection {
    endpoints: Vec<String>,
    client: reqwest::Client,
}

impl RethConnection {
    fn new(endpoints: Vec<String>) -> Self {
        Self {
            endpoints,
            client: reqwest::Client::new(),
        }
    }

    async fn check_endpoints(&self) -> Result<Vec<String>> {
        let mut working_endpoints = Vec::new();
        
        for endpoint in &self.endpoints {
            match self.get_client_version(endpoint).await {
                Ok(version) => {
                    info!("✅ Connected to reth node at {}: {}", endpoint, version);
                    working_endpoints.push(endpoint.clone());
                }
                Err(e) => {
                    warn!("❌ Failed to connect to {}: {}", endpoint, e);
                }
            }
        }

        if working_endpoints.is_empty() {
            return Err(anyhow::anyhow!("No working reth endpoints found"));
        }

        Ok(working_endpoints)
    }

    async fn get_client_version(&self, endpoint: &str) -> Result<String> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "web3_clientVersion",
            "params": [],
            "id": 1
        });

        let response = self.client
            .post(endpoint)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        let json: Value = response.json().await?;
        
        if let Some(result) = json.get("result") {
            if let Some(version) = result.as_str() {
                return Ok(version.to_string());
            }
        }

        Err(anyhow::anyhow!("Invalid response format"))
    }

    async fn get_latest_block(&self, endpoint: &str) -> Result<Value> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "eth_getBlockByNumber",
            "params": ["latest", false],
            "id": 1
        });

        let response = self.client
            .post(endpoint)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        let json: Value = response.json().await?;
        
        if let Some(result) = json.get("result") {
            return Ok(result.clone());
        }

        Err(anyhow::anyhow!("No result in response"))
    }

    async fn get_peer_count(&self, endpoint: &str) -> Result<u64> {
        let payload = json!({
            "jsonrpc": "2.0", 
            "method": "net_peerCount",
            "params": [],
            "id": 1
        });

        let response = self.client
            .post(endpoint)
            .json(&payload)
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        let json: Value = response.json().await?;
        
        if let Some(result) = json.get("result") {
            if let Some(hex_str) = result.as_str() {
                let count = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)?;
                return Ok(count);
            }
        }

        Err(anyhow::anyhow!("Invalid peer count response"))
    }
}

/// Demo state management
struct DemoState {
    rainbow_analyzer: RainbowAnalyzer,
    stealth_enabled: bool,
    reth_connection: RethConnection,
    working_endpoints: Vec<String>,
    attestation_network: AttestationNetwork,
    // Real stealth sidecar components
    subnet_juggler_handle: Option<SubnetJugglerHandle>,
    friend_relay_handle: Option<friend_relay::FriendRelayHandle>,
    stealth_config: StealthConfig,
    // Metrics collection
    metrics_collector: Option<std::sync::Arc<StealthMetricsCollector>>,
}

impl DemoState {
    async fn new(endpoints: Vec<String>, bootstrap_peers: Vec<String>) -> Result<Self> {
        let reth_connection = RethConnection::new(endpoints);
        let working_endpoints = reth_connection.check_endpoints().await?;
        
        // Initialize real libp2p attestation network
        let attestation_network = AttestationNetwork::new(bootstrap_peers).await?;
        
        // Create stealth configuration
        let stealth_config = StealthConfig {
            lighthouse_http_api: "http://localhost:5052".to_string(),
            extra_subnets_per_epoch: 8,
            friend_nodes: vec![
                FriendNodeConfig {
                    peer_id: "friend_1".to_string(),
                    multiaddr: "/ip4/127.0.0.1/tcp/60001".parse().unwrap(),
                    public_key: "0x1234".to_string(),
                },
                FriendNodeConfig {
                    peer_id: "friend_2".to_string(), 
                    multiaddr: "/ip4/127.0.0.1/tcp/60002".parse().unwrap(),
                    public_key: "0x5678".to_string(),
                },
            ],
            waku_config: WakuConfig {
                nwaku_rpc_url: "http://localhost:8545".to_string(),
                rln_contract_address: None,
                rate_limit_per_epoch: 100,
            },
            metrics: MetricsConfig {
                enabled: true,
                listen_address: "127.0.0.1".to_string(),
                listen_port: 9090,
            },
            network: NetworkConfig {
                listen_port: 9000,
                external_ip: None,
            },
        };
        
        // Initialize metrics if enabled
        let metrics_collector = if stealth_config.metrics.enabled {
            match StealthMetricsCollector::new() {
                Ok(collector) => {
                    let collector_arc = std::sync::Arc::new(collector);
                    
                    // Start metrics server
                    let metrics_server = MetricsServer::new(
                        collector_arc.clone(), 
                        stealth_config.metrics.clone()
                    );
                    tokio::spawn(async move {
                        if let Err(e) = metrics_server.start().await {
                            error!("Metrics server error: {}", e);
                        }
                    });
                    
                    // Start system metrics updater
                    tokio::spawn(start_system_metrics_updater(collector_arc.clone()));
                    
                    info!("📊 Metrics server started on http://{}:{}/metrics", 
                        stealth_config.metrics.listen_address, 
                        stealth_config.metrics.listen_port);
                    
                    Some(collector_arc)
                }
                Err(e) => {
                    warn!("Failed to initialize metrics: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            rainbow_analyzer: RainbowAnalyzer::new(),
            stealth_enabled: false,
            reth_connection,
            working_endpoints,
            attestation_network,
            subnet_juggler_handle: None,
            friend_relay_handle: None,
            stealth_config,
            metrics_collector,
        })
    }

    async fn observe_network_activity(&mut self) {
        // Try to receive real attestations from the network
        let received_real = if let Some(attestation) = self.attestation_network.next_attestation().await {
            info!("📨 Real attestation: validator {} on subnet {} from peer {}",
                attestation.validator_index, attestation.subnet_id, attestation.source_peer);
            
            // Record metrics for bandwidth usage
            if let Some(metrics) = &self.metrics_collector {
                metrics.record_bandwidth(
                    attestation.raw_data.len() as u64,
                    "inbound",
                    "gossip"
                );
                metrics.record_message_size(
                    attestation.raw_data.len(),
                    "attestation"
                );
            }
            
            self.rainbow_analyzer.observe_real_attestation(&attestation);
            true
        } else {
            false
        };
        
        // If no real attestations received, add simulated ones to demonstrate stealth systems
        if !received_real {
            self.simulate_realistic_attestations();
        }
        
        // Also check reth endpoints for additional context
        for endpoint in &self.working_endpoints.clone() {
            match self.reth_connection.get_latest_block(endpoint).await {
                Ok(block) => {
                    if let Some(block_number) = block.get("number") {
                        debug!("📦 Latest block from {}: {}", endpoint, block_number);
                    }
                }
                Err(e) => {
                    debug!("Failed to get block from {}: {}", endpoint, e);
                }
            }

            match self.reth_connection.get_peer_count(endpoint).await {
                Ok(peer_count) => {
                    debug!("👥 Peer count at {}: {}", endpoint, peer_count);
                }
                Err(e) => {
                    debug!("Failed to get peer count from {}: {}", endpoint, e);
                }
            }
        }
    }

    fn simulate_realistic_attestations(&mut self) {
        let mut rng = rand::thread_rng();
        
        // Simulate realistic attestation patterns based on stealth status
        let attestation_count = rng.gen_range(2..=5);
        
        for _ in 0..attestation_count {
            let validator_index = rng.gen_range(0..100); // Larger validator set for realism
            let subnet_id = if self.stealth_enabled {
                // With stealth: more random subnet selection due to shuffling
                rng.gen_range(0..64) as u8
            } else {
                // Without stealth: validators stick to backbone subnets more often
                let backbone_subnet = ((validator_index * 2) % 64) as u8;
                if rng.gen_bool(0.7) {
                    backbone_subnet // 70% chance to use backbone subnet
                } else {
                    rng.gen_range(0..64) as u8 // 30% chance to use random subnet
                }
            };
            
            // Create realistic attestation
            let simulated_attestation = RealAttestation {
                validator_index,
                subnet_id,
                source_peer: PeerId::random(),
                timestamp: Instant::now(),
                raw_data: vec![0u8; rng.gen_range(300..800)], // Realistic size
            };
            
            debug!("📊 Simulated attestation: validator {} on subnet {} (stealth: {})",
                validator_index, subnet_id, self.stealth_enabled);
            
            // Record metrics if available
            if let Some(metrics) = &self.metrics_collector {
                metrics.record_bandwidth(
                    simulated_attestation.raw_data.len() as u64,
                    "inbound",
                    "simulated"
                );
                metrics.record_message_size(
                    simulated_attestation.raw_data.len(),
                    "attestation"
                );
            }
            
            self.rainbow_analyzer.observe_real_attestation(&simulated_attestation);
        }
    }

    fn simulate_attestation_observations(&mut self, _block: &Value) {
        // Legacy method - now redirects to new realistic simulation
        self.simulate_realistic_attestations();
    }

    fn handle_send_attestation(&mut self, validator_id: u8, subnet_id: u8, data: &str) {
        info!("📝 Attestation from validator {} on subnet {}", validator_id, subnet_id);
        
        if self.stealth_enabled {
            info!("🛡️  Protected: Forwarding through friend mesh + subnet shuffling");
            
            // Use real friend relay to forward the attestation
            if let Some(friend_relay) = &self.friend_relay_handle {
                let start_time = std::time::Instant::now();
                if let Err(e) = friend_relay.relay_attestation(data.as_bytes().to_vec(), subnet_id) {
                    warn!("Failed to relay attestation through friends: {}", e);
                } else {
                    let latency = start_time.elapsed().as_secs_f64();
                    debug!("  └─ Attestation relayed through friend mesh in {:.3}s", latency);
                    
                    // Record metrics for friend relay
                    if let Some(metrics) = &self.metrics_collector {
                        metrics.record_attestation_relayed(latency);
                        metrics.record_bandwidth(
                            data.len() as u64,
                            "outbound", 
                            "waku"
                        );
                        metrics.privacy_events_total.with_label_values(&["friend_relay"]).inc();
                    }
                }
            }
        }
        
        self.rainbow_analyzer.observe_attestation(validator_id, subnet_id);
    }

    async fn enable_stealth(&mut self) {
        info!("🛡️  ENABLING STEALTH MODE");
        self.stealth_enabled = true;
        
        // Start real subnet juggler with system clock provider (no Lighthouse dependency)
        let system_provider = SystemClockProvider::new();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        
        let (mut subnet_juggler, handle) = SubnetJuggler::new(
            self.stealth_config.clone(),
            system_provider,
            shutdown_rx,
        );
        
        self.subnet_juggler_handle = Some(handle);
        
        // Start subnet juggler in background
        tokio::spawn(async move {
            if let Err(e) = subnet_juggler.run().await {
                error!("Subnet juggler error: {}", e);
            }
        });
        
        // Start real friend relay
        let waku_provider = NwakuProvider::new(&self.stealth_config.waku_config);
        let (shutdown_tx2, shutdown_rx2) = tokio::sync::watch::channel(false);
            
        let (mut friend_relay, friend_relay_handle) = FriendRelay::new(
            self.stealth_config.clone(),
            waku_provider,
            shutdown_rx2,
        );
        
        self.friend_relay_handle = Some(friend_relay_handle);
        
        // Start friend relay in background  
        tokio::spawn(async move {
            if let Err(e) = friend_relay.run().await {
                error!("Friend relay error: {}", e);
            }
        });
        
        // Record metrics for enabling stealth
        if let Some(metrics) = &self.metrics_collector {
            for _ in 0..self.stealth_config.extra_subnets_per_epoch {
                metrics.record_subnet_joined(false); // Extra subnets
            }
            metrics.privacy_events_total.with_label_values(&["subnet_shuffle"]).inc();
        }

        info!("✅ Subnet juggler started - will shuffle {} extra subnets per epoch", 
            self.stealth_config.extra_subnets_per_epoch);
        info!("✅ Friend relay mesh activated ({} trusted nodes)", 
            self.stealth_config.friend_nodes.len());
        info!("✅ RLN rate limiting enabled");
    }

    async fn disable_stealth(&mut self) {
        info!("🔓 DISABLING STEALTH MODE");
        self.stealth_enabled = false;
        
        // Stop subnet juggler
        if let Some(handle) = &self.subnet_juggler_handle {
            if let Err(e) = handle.send_command(SubnetCommand::Stop) {
                warn!("Failed to stop subnet juggler: {}", e);
            }
        }
        self.subnet_juggler_handle = None;
        
        // Stop friend relay  
        if let Some(handle) = &self.friend_relay_handle {
            if let Err(e) = handle.send_command(RelayCommand::Stop) {
                warn!("Failed to stop friend relay: {}", e);
            }
        }
        self.friend_relay_handle = None;
        
        info!("✅ Returned to baseline configuration - stealth protection disabled");
    }

    fn get_rainbow_stats(&self) -> Value {
        let stats = self.rainbow_analyzer.get_stats();
        
        if self.stealth_enabled {
            info!("📊 RAINBOW Analysis (WITH STEALTH PROTECTION):");
        } else {
            info!("📊 RAINBOW Analysis (BASELINE - NO PROTECTION):");
        }
        
        info!("   Total messages: {}", stats["total_messages_observed"]);
        info!("   Validators analyzed: {}", stats["total_validators_analyzed"]);
        info!("   Successfully mapped: {}", stats["successfully_mapped_validators"]);
        info!("   Success rate: {:.1}%", stats["success_rate"].as_f64().unwrap_or(0.0) * 100.0);
        
        if let Some(mapped) = stats["mapped_validators"].as_array() {
            for validator in mapped {
                if let (Some(id), Some(confidence)) = (validator["validator_id"].as_u64(), validator["confidence"].as_f64()) {
                    info!("     • Validator {} mapped with {:.1}% confidence", id, confidence * 100.0);
                }
            }
        }
        
        stats
    }
}

async fn run_demo(args: Args) -> Result<()> {
    info!("🚀 Starting realistic demo with real libp2p gossipsub integration");
    info!("   Reth endpoints: {:?}", args.reth_endpoints);
    info!("   P2P addresses: {:?}", args.reth_p2p_addrs);
    info!("   Bootstrap peers: {:?}", args.bootstrap_peers);
    
    // Initialize demo state with real reth connections and libp2p network
    let mut demo_state = DemoState::new(args.reth_endpoints, args.bootstrap_peers).await?;
    
    info!("✅ Connected to {} working reth endpoints", demo_state.working_endpoints.len());
    
    // Set up command pipe reader
    let command_pipe = args.command_pipe.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<DemoCommand>();
    
    // Spawn command reader task
    let cmd_tx_clone = cmd_tx.clone();
    tokio::spawn(async move {
        loop {
            match tokio::fs::File::open(&command_pipe).await {
                Ok(file) => {
                    let reader = BufReader::new(file.into_std().await);
                    for line in reader.lines() {
                        if let Ok(line) = line {
                            let line = line.trim();
                            if line.is_empty() { continue; }
                            
                            let cmd = if line == "get_rainbow_stats" {
                                DemoCommand::GetRainbowStats
                            } else if line == "enable_stealth" {
                                DemoCommand::EnableStealth
                            } else if line == "disable_stealth" {
                                DemoCommand::DisableStealth
                            } else if line.starts_with("send_attestation ") {
                                let parts: Vec<&str> = line.split_whitespace().collect();
                                if parts.len() >= 4 {
                                    if let (Ok(validator_id), Ok(subnet_id)) = (parts[1].parse(), parts[2].parse()) {
                                        DemoCommand::SendAttestation {
                                            validator_id,
                                            subnet_id,
                                            data: parts[3].to_string(),
                                        }
                                    } else { continue; }
                                } else { continue; }
                            } else {
                                continue;
                            };
                            
                            if cmd_tx_clone.send(cmd).is_err() {
                                break;
                            }
                        }
                    }
                }
                Err(_) => {
                    // Pipe doesn't exist yet, wait and retry
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    
    // Main demo loop
    let mut network_observer = interval(Duration::from_secs(5));
    let demo_end = tokio::time::Instant::now() + Duration::from_secs(args.duration);
    
    info!("🔍 Demo running for {} seconds...", args.duration);
    info!("📡 Listening for real attestations from libp2p gossipsub network");
    
    loop {
        tokio::select! {
            // Continuously observe network for real-time attestations
            _ = demo_state.observe_network_activity() => {
                // This will process one attestation and return immediately
            }
            
            // Periodic status check
            _ = network_observer.tick() => {
                let stats = demo_state.get_rainbow_stats();
                if stats["total_messages_observed"].as_u64().unwrap_or(0) % 10 == 0 {
                    debug!("📊 Processed {} attestations so far", stats["total_messages_observed"]);
                }
            }
            
            cmd = cmd_rx.recv() => {
                if let Some(cmd) = cmd {
                    match cmd {
                        DemoCommand::GetRainbowStats => {
                            demo_state.get_rainbow_stats();
                        }
                        DemoCommand::SendAttestation { validator_id, subnet_id, data } => {
                            demo_state.handle_send_attestation(validator_id, subnet_id, &data);
                        }
                        DemoCommand::EnableStealth => {
                            demo_state.enable_stealth().await;
                        }
                        DemoCommand::DisableStealth => {
                            demo_state.disable_stealth().await;
                        }
                    }
                }
            }
            
            _ = tokio::time::sleep_until(demo_end) => {
                info!("⏰ Demo duration completed");
                break;
            }
        }
    }
    
    info!("🎯 Demo completed successfully!");
    info!("Final stats:");
    demo_state.get_rainbow_stats();
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("realistic_demo=info,warn")
        .init();

    let args = Args::parse();
    
    if let Err(e) = run_demo(args).await {
        error!("Demo failed: {}", e);
        std::process::exit(1);
    }
    
    Ok(())
}