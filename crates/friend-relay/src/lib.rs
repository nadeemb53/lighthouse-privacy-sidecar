use alloy_primitives::B256;
use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use libp2p::PeerId;
use rand::seq::SliceRandom;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use stealth_common::{FriendNodeConfig, StealthConfig, StealthError, StealthResult, WakuConfig};
use tokio::sync::{mpsc, watch, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

/// A message to be relayed through the friend mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayMessage {
    /// Unique identifier for this message
    pub message_id: String,
    /// The original attestation data
    pub attestation_data: Vec<u8>,
    /// Subnet this attestation belongs to
    pub subnet_id: u8,
    /// Timestamp when message was created
    pub timestamp: DateTime<Utc>,
    /// Origin peer (encrypted or anonymized)
    pub origin_hint: Option<String>,
}

/// RLN (Rate Limiting Nullifier) proof for spam protection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlnProof {
    /// The nullifier for this epoch
    pub nullifier: B256,
    /// The proof data
    pub proof: Vec<u8>,
    /// RLN epoch
    pub epoch: u64,
    /// Signal hash
    pub signal_hash: B256,
}

/// A message with its RLN proof for transmission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenMessage {
    pub message: RelayMessage,
    pub rln_proof: RlnProof,
    pub sender_id: String,
}

/// Commands that can be sent to the FriendRelay
#[derive(Debug, Clone)]
pub enum RelayCommand {
    /// Relay an attestation through friends
    RelayAttestation {
        attestation_data: Vec<u8>,
        subnet_id: u8,
    },
    /// Add a new friend node
    AddFriend(FriendNodeConfig),
    /// Remove a friend node
    RemoveFriend(String), // peer_id
    /// Get relay statistics
    GetStats,
    /// Update RLN parameters
    UpdateRln {
        rate_limit: u32,
        epoch: u64,
    },
    /// Stop the relay
    Stop,
}

/// Events emitted by the FriendRelay
#[derive(Debug, Clone)]
pub enum RelayEvent {
    /// Message was successfully relayed
    MessageRelayed {
        message_id: String,
        friends_count: usize,
        latency_ms: u64,
    },
    /// Message was received from a friend
    MessageReceived {
        message_id: String,
        from_friend: String,
    },
    /// Friend node connected
    FriendConnected(String),
    /// Friend node disconnected
    FriendDisconnected(String),
    /// RLN rate limit exceeded
    RateLimitExceeded {
        epoch: u64,
        attempts: u32,
        limit: u32,
    },
    /// Error occurred
    Error(String),
}

/// Statistics for the friend relay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub friends_connected: usize,
    pub average_latency_ms: f64,
    pub rate_limit_violations: u64,
    pub bandwidth_bytes_per_second: f64,
}

/// Interface to interact with Waku node for RLN proofs and message relay
#[async_trait::async_trait]
pub trait WakuProvider: Send + Sync {
    /// Generate an RLN proof for a message
    async fn generate_rln_proof(
        &self,
        message: &[u8],
        epoch: u64,
    ) -> StealthResult<RlnProof>;

    /// Verify an RLN proof
    async fn verify_rln_proof(
        &self,
        proof: &RlnProof,
        message: &[u8],
    ) -> StealthResult<bool>;

    /// Send a message via Waku LightPush
    async fn light_push(
        &self,
        topic: &str,
        message: &[u8],
        timestamp: Option<DateTime<Utc>>,
    ) -> StealthResult<String>;

    /// Subscribe to messages on a topic via Waku Relay
    async fn subscribe_relay(
        &self,
        topic: &str,
    ) -> StealthResult<mpsc::UnboundedReceiver<Vec<u8>>>;

    /// Get current RLN epoch
    async fn get_rln_epoch(&self) -> StealthResult<u64>;
}

/// Implementation of WakuProvider that talks to nwaku via JSON-RPC
pub struct NwakuProvider {
    client: reqwest::Client,
    base_url: String,
    rln_contract_address: Option<String>,
}

impl NwakuProvider {
    pub fn new(config: &WakuConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: config.nwaku_rpc_url.clone(),
            rln_contract_address: config.rln_contract_address.clone(),
        }
    }

    async fn rpc_call<T>(&self, method: &str, params: serde_json::Value) -> StealthResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });

        let response = self
            .client
            .post(&self.base_url)
            .json(&payload)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| StealthError::WakuRln(format!("RPC call failed: {}", e)))?;

        #[derive(Deserialize)]
        struct RpcResponse<T> {
            result: Option<T>,
            error: Option<serde_json::Value>,
        }

        let rpc_response: RpcResponse<T> = response
            .json()
            .await
            .map_err(|e| StealthError::WakuRln(format!("JSON decode failed: {}", e)))?;

        if let Some(error) = rpc_response.error {
            return Err(StealthError::WakuRln(format!("RPC error: {}", error)));
        }

        rpc_response
            .result
            .ok_or_else(|| StealthError::WakuRln("No result in RPC response".to_string()))
    }
}

#[async_trait::async_trait]
impl WakuProvider for NwakuProvider {
    async fn generate_rln_proof(
        &self,
        message: &[u8],
        epoch: u64,
    ) -> StealthResult<RlnProof> {
        let message_b64 = general_purpose::STANDARD.encode(message);
        
        let params = serde_json::json!({
            "message": message_b64,
            "epoch": epoch
        });

        #[derive(Deserialize)]
        struct ProofResponse {
            nullifier: String,
            proof: String,
            signal_hash: String,
        }

        let response: ProofResponse = self.rpc_call("rln_generateProof", params).await?;

        // Decode hex strings
        let nullifier = B256::from_slice(
            &hex::decode(response.nullifier.trim_start_matches("0x"))
                .map_err(|e| StealthError::WakuRln(format!("Invalid nullifier hex: {}", e)))?
        );

        let proof = general_purpose::STANDARD
            .decode(response.proof)
            .map_err(|e| StealthError::WakuRln(format!("Invalid proof base64: {}", e)))?;

        let signal_hash = B256::from_slice(
            &hex::decode(response.signal_hash.trim_start_matches("0x"))
                .map_err(|e| StealthError::WakuRln(format!("Invalid signal hash hex: {}", e)))?
        );

        Ok(RlnProof {
            nullifier,
            proof,
            epoch,
            signal_hash,
        })
    }

    async fn verify_rln_proof(
        &self,
        proof: &RlnProof,
        message: &[u8],
    ) -> StealthResult<bool> {
        let message_b64 = general_purpose::STANDARD.encode(message);
        let proof_b64 = general_purpose::STANDARD.encode(&proof.proof);

        let params = serde_json::json!({
            "proof": proof_b64,
            "message": message_b64,
            "nullifier": format!("0x{}", hex::encode(proof.nullifier.as_slice())),
            "epoch": proof.epoch,
            "signal_hash": format!("0x{}", hex::encode(proof.signal_hash.as_slice()))
        });

        #[derive(Deserialize)]
        struct VerifyResponse {
            valid: bool,
        }

        let response: VerifyResponse = self.rpc_call("rln_verifyProof", params).await?;
        Ok(response.valid)
    }

    async fn light_push(
        &self,
        topic: &str,
        message: &[u8],
        timestamp: Option<DateTime<Utc>>,
    ) -> StealthResult<String> {
        let message_b64 = general_purpose::STANDARD.encode(message);
        let timestamp_ns = timestamp
            .unwrap_or_else(Utc::now)
            .timestamp_nanos_opt()
            .unwrap_or(0);

        let params = serde_json::json!({
            "pubsubTopic": topic,
            "message": {
                "payload": message_b64,
                "timestamp": timestamp_ns
            }
        });

        #[derive(Deserialize)]
        struct PushResponse {
            message_id: String,
        }

        let response: PushResponse = self.rpc_call("relay_v1_lightpush", params).await?;
        Ok(response.message_id)
    }

    async fn subscribe_relay(
        &self,
        topic: &str,
    ) -> StealthResult<mpsc::UnboundedReceiver<Vec<u8>>> {
        // In a real implementation, this would set up a WebSocket or SSE connection
        // to receive real-time messages. For this demo, we'll simulate it.
        let (tx, rx) = mpsc::unbounded_channel();
        
        debug!("Subscribed to Waku topic: {}", topic);
        
        // In practice, you'd start a background task that listens for messages
        // and forwards them to the channel
        
        Ok(rx)
    }

    async fn get_rln_epoch(&self) -> StealthResult<u64> {
        #[derive(Deserialize)]
        struct EpochResponse {
            epoch: u64,
        }

        let response: EpochResponse = self.rpc_call("rln_currentEpoch", serde_json::json!({})).await?;
        Ok(response.epoch)
    }
}

/// Rate limiter that tracks message counts per epoch
pub struct RateLimiter {
    rate_limit_per_epoch: u32,
    current_epoch: u64,
    message_count: u32,
    nullifiers_seen: HashMap<u64, Vec<B256>>, // epoch -> nullifiers
}

impl RateLimiter {
    pub fn new(rate_limit_per_epoch: u32) -> Self {
        Self {
            rate_limit_per_epoch,
            current_epoch: 0,
            message_count: 0,
            nullifiers_seen: HashMap::new(),
        }
    }

    pub fn check_and_update(&mut self, epoch: u64, nullifier: B256) -> Result<(), String> {
        // Update epoch if necessary
        if epoch > self.current_epoch {
            self.current_epoch = epoch;
            self.message_count = 0;
            // Clean up old nullifiers (keep last 3 epochs)
            self.nullifiers_seen.retain(|&e, _| e >= epoch.saturating_sub(3));
        }

        // Check for nullifier reuse (double-spending protection)
        let epoch_nullifiers = self.nullifiers_seen.entry(epoch).or_insert_with(Vec::new);
        if epoch_nullifiers.contains(&nullifier) {
            return Err(format!("Nullifier already used in epoch {}", epoch));
        }

        // Check rate limit
        if self.message_count >= self.rate_limit_per_epoch {
            return Err(format!(
                "Rate limit exceeded: {}/{} messages in epoch {}",
                self.message_count, self.rate_limit_per_epoch, epoch
            ));
        }

        // Update counters
        self.message_count += 1;
        epoch_nullifiers.push(nullifier);

        Ok(())
    }

    pub fn get_stats(&self) -> (u64, u32, u32) {
        (self.current_epoch, self.message_count, self.rate_limit_per_epoch)
    }
}

/// Message queue with deduplication
pub struct MessageQueue {
    max_size: usize,
    messages: VecDeque<(String, Instant)>, // (message_id, timestamp)
}

impl MessageQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            messages: VecDeque::new(),
        }
    }

    pub fn has_seen(&mut self, message_id: &str) -> bool {
        // Clean up old messages (older than 5 minutes)
        let cutoff = Instant::now() - Duration::from_secs(300);
        while let Some((_, timestamp)) = self.messages.front() {
            if *timestamp < cutoff {
                self.messages.pop_front();
            } else {
                break;
            }
        }

        // Check if we've seen this message
        self.messages.iter().any(|(id, _)| id == message_id)
    }

    pub fn add_message(&mut self, message_id: String) {
        if self.messages.len() >= self.max_size {
            self.messages.pop_front();
        }
        self.messages.push_back((message_id, Instant::now()));
    }
}

/// The main FriendRelay component that manages attestation forwarding through friends
pub struct FriendRelay<W: WakuProvider> {
    config: StealthConfig,
    waku_provider: W,
    friends: HashMap<String, FriendNodeConfig>,
    rate_limiter: Arc<RwLock<RateLimiter>>,
    message_queue: Arc<RwLock<MessageQueue>>,
    stats: Arc<RwLock<RelayStats>>,
    command_tx: mpsc::UnboundedSender<RelayCommand>,
    command_rx: mpsc::UnboundedReceiver<RelayCommand>,
    event_tx: mpsc::UnboundedSender<RelayEvent>,
    shutdown_rx: watch::Receiver<bool>,
}

impl<W: WakuProvider> FriendRelay<W> {
    pub fn new(
        config: StealthConfig,
        waku_provider: W,
        shutdown_rx: watch::Receiver<bool>,
    ) -> (Self, FriendRelayHandle) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let friends: HashMap<String, FriendNodeConfig> = config
            .friend_nodes
            .iter()
            .map(|f| (f.peer_id.clone(), f.clone()))
            .collect();

        let relay = Self {
            config: config.clone(),
            waku_provider,
            friends,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new(
                config.waku_config.rate_limit_per_epoch,
            ))),
            message_queue: Arc::new(RwLock::new(MessageQueue::new(1000))),
            stats: Arc::new(RwLock::new(RelayStats {
                messages_sent: 0,
                messages_received: 0,
                friends_connected: 0,
                average_latency_ms: 0.0,
                rate_limit_violations: 0,
                bandwidth_bytes_per_second: 0.0,
            })),
            command_tx: command_tx.clone(),
            command_rx,
            event_tx: event_tx.clone(),
            shutdown_rx,
        };

        let handle = FriendRelayHandle {
            command_tx,
            event_rx,
        };

        (relay, handle)
    }

    /// Main event loop for the friend relay
    pub async fn run(&mut self) -> StealthResult<()> {
        info!("Starting FriendRelay with {} friends...", self.friends.len());

        // Set up periodic tasks
        let mut stats_timer = interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                // Check for shutdown signal
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("Shutdown signal received, stopping FriendRelay");
                        break;
                    }
                }

                // Handle periodic stats update
                _ = stats_timer.tick() => {
                    self.update_stats().await;
                }

                // Handle commands
                Some(command) = self.command_rx.recv() => {
                    if let Err(e) = self.handle_command(command).await {
                        error!("Error handling command: {}", e);
                        let _ = self.event_tx.send(RelayEvent::Error(e.to_string()));
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_command(&mut self, command: RelayCommand) -> StealthResult<()> {
        match command {
            RelayCommand::RelayAttestation { attestation_data, subnet_id } => {
                self.relay_attestation(attestation_data, subnet_id).await?;
            }
            RelayCommand::AddFriend(friend_config) => {
                info!("Adding friend: {}", friend_config.peer_id);
                self.friends.insert(friend_config.peer_id.clone(), friend_config.clone());
                let _ = self.event_tx.send(RelayEvent::FriendConnected(friend_config.peer_id));
            }
            RelayCommand::RemoveFriend(peer_id) => {
                info!("Removing friend: {}", peer_id);
                self.friends.remove(&peer_id);
                let _ = self.event_tx.send(RelayEvent::FriendDisconnected(peer_id));
            }
            RelayCommand::GetStats => {
                let stats = self.stats.read().await.clone();
                debug!("Current relay stats: {:?}", stats);
            }
            RelayCommand::UpdateRln { rate_limit, epoch } => {
                info!("Updating RLN parameters: rate_limit={}, epoch={}", rate_limit, epoch);
                let mut limiter = self.rate_limiter.write().await;
                *limiter = RateLimiter::new(rate_limit);
            }
            RelayCommand::Stop => {
                info!("Received stop command");
                return Err(StealthError::WakuRln("Stop requested".to_string()));
            }
        }
        Ok(())
    }

    async fn relay_attestation(&mut self, attestation_data: Vec<u8>, subnet_id: u8) -> StealthResult<()> {
        let start_time = Instant::now();
        
        // Create relay message
        let message_id = format!("{}_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0), subnet_id);
        let relay_message = RelayMessage {
            message_id: message_id.clone(),
            attestation_data: attestation_data.clone(),
            subnet_id,
            timestamp: Utc::now(),
            origin_hint: None, // We don't reveal origin for privacy
        };

        // Get current RLN epoch
        let rln_epoch = self.waku_provider.get_rln_epoch().await?;

        // Generate RLN proof
        let message_bytes = serde_json::to_vec(&relay_message)
            .map_err(|e| StealthError::WakuRln(format!("Message serialization failed: {}", e)))?;
        
        let rln_proof = self.waku_provider.generate_rln_proof(&message_bytes, rln_epoch).await?;

        // Check rate limiter
        {
            let mut limiter = self.rate_limiter.write().await;
            if let Err(e) = limiter.check_and_update(rln_epoch, rln_proof.nullifier) {
                warn!("Rate limit check failed: {}", e);
                let (epoch, count, limit) = limiter.get_stats();
                let _ = self.event_tx.send(RelayEvent::RateLimitExceeded {
                    epoch,
                    attempts: count,
                    limit,
                });
                return Err(StealthError::WakuRln(e));
            }
        }

        // Create proven message
        let proven_message = ProvenMessage {
            message: relay_message,
            rln_proof,
            sender_id: "stealth_sidecar".to_string(), // Anonymous sender ID
        };

        let proven_message_bytes = serde_json::to_vec(&proven_message)
            .map_err(|e| StealthError::WakuRln(format!("Proven message serialization failed: {}", e)))?;

        // Select friends to relay through (randomize for better privacy)
        let mut selected_friends: Vec<_> = self.friends.values().cloned().collect();
        selected_friends.shuffle(&mut thread_rng());
        
        // Use all friends for maximum k-anonymity
        let friends_count = selected_friends.len();
        
        // Relay to all friends simultaneously
        let relay_futures: Vec<_> = selected_friends
            .iter()
            .map(|friend| {
                let topic = format!("/stealth-relay/{}/ssz", subnet_id);
                let waku_provider = &self.waku_provider;
                let message_bytes = proven_message_bytes.clone();
                async move {
                    waku_provider.light_push(&topic, &message_bytes, Some(Utc::now())).await
                }
            })
            .collect();

        // Wait for all relays to complete
        let results = futures::future::join_all(relay_futures).await;
        
        // Count successful relays
        let successful_relays = results.iter().filter(|r| r.is_ok()).count();
        
        if successful_relays == 0 {
            return Err(StealthError::WakuRln("Failed to relay to any friends".to_string()));
        }

        if successful_relays < friends_count {
            warn!("Only {}/{} friends received the message", successful_relays, friends_count);
        }

        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.messages_sent += 1;
            let latency_ms = start_time.elapsed().as_millis() as u64;
            stats.average_latency_ms = 
                (stats.average_latency_ms * (stats.messages_sent - 1) as f64 + latency_ms as f64) 
                / stats.messages_sent as f64;
        }

        // Emit success event
        let _ = self.event_tx.send(RelayEvent::MessageRelayed {
            message_id,
            friends_count: successful_relays,
            latency_ms: start_time.elapsed().as_millis() as u64,
        });

        info!("Relayed attestation to {}/{} friends in {}ms", 
              successful_relays, friends_count, start_time.elapsed().as_millis());

        Ok(())
    }

    async fn update_stats(&self) {
        let stats = self.stats.read().await;
        debug!("Friend relay stats: sent={}, received={}, friends={}, avg_latency={}ms",
               stats.messages_sent, stats.messages_received, 
               stats.friends_connected, stats.average_latency_ms);
    }

    pub async fn get_stats(&self) -> RelayStats {
        self.stats.read().await.clone()
    }
}

/// Handle to interact with the FriendRelay
pub struct FriendRelayHandle {
    command_tx: mpsc::UnboundedSender<RelayCommand>,
    event_rx: mpsc::UnboundedReceiver<RelayEvent>,
}

impl FriendRelayHandle {
    /// Send a command to the FriendRelay
    pub fn send_command(&self, command: RelayCommand) -> StealthResult<()> {
        self.command_tx
            .send(command)
            .map_err(|_| StealthError::WakuRln("Channel closed".to_string()))
    }

    /// Receive events from the FriendRelay
    pub async fn recv_event(&mut self) -> Option<RelayEvent> {
        self.event_rx.recv().await
    }

    /// Relay an attestation through the friend mesh
    pub fn relay_attestation(&self, attestation_data: Vec<u8>, subnet_id: u8) -> StealthResult<()> {
        self.send_command(RelayCommand::RelayAttestation { attestation_data, subnet_id })
    }

    /// Add a new friend node
    pub fn add_friend(&self, friend_config: FriendNodeConfig) -> StealthResult<()> {
        self.send_command(RelayCommand::AddFriend(friend_config))
    }

    /// Remove a friend node
    pub fn remove_friend(&self, peer_id: String) -> StealthResult<()> {
        self.send_command(RelayCommand::RemoveFriend(peer_id))
    }

    /// Get relay statistics
    pub fn get_stats(&self) -> StealthResult<()> {
        self.send_command(RelayCommand::GetStats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    struct MockWakuProvider {
        current_epoch: u64,
    }

    impl MockWakuProvider {
        fn new() -> Self {
            Self { current_epoch: 100 }
        }
    }

    #[async_trait::async_trait]
    impl WakuProvider for MockWakuProvider {
        async fn generate_rln_proof(&self, _message: &[u8], epoch: u64) -> StealthResult<RlnProof> {
            Ok(RlnProof {
                nullifier: B256::random(),
                proof: vec![1, 2, 3, 4],
                epoch,
                signal_hash: B256::random(),
            })
        }

        async fn verify_rln_proof(&self, _proof: &RlnProof, _message: &[u8]) -> StealthResult<bool> {
            Ok(true)
        }

        async fn light_push(&self, _topic: &str, _message: &[u8], _timestamp: Option<DateTime<Utc>>) -> StealthResult<String> {
            Ok("message_123".to_string())
        }

        async fn subscribe_relay(&self, _topic: &str) -> StealthResult<mpsc::UnboundedReceiver<Vec<u8>>> {
            let (_tx, rx) = mpsc::unbounded_channel();
            Ok(rx)
        }

        async fn get_rln_epoch(&self) -> StealthResult<u64> {
            Ok(self.current_epoch)
        }
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let mut limiter = RateLimiter::new(10);
        
        // Should allow messages up to the limit
        for i in 0..10 {
            let nullifier = B256::random();
            assert!(limiter.check_and_update(100, nullifier).is_ok(), "Message {} should be allowed", i);
        }
        
        // Should reject after limit
        let nullifier = B256::random();
        assert!(limiter.check_and_update(100, nullifier).is_err());
        
        // Should reset for new epoch
        let nullifier = B256::random();
        assert!(limiter.check_and_update(101, nullifier).is_ok());
    }

    #[tokio::test]
    async fn test_message_queue_deduplication() {
        let mut queue = MessageQueue::new(10);
        
        // First message should be new
        assert!(!queue.has_seen("msg1"));
        queue.add_message("msg1".to_string());
        
        // Second instance should be detected
        assert!(queue.has_seen("msg1"));
        
        // Different message should be new
        assert!(!queue.has_seen("msg2"));
    }

    #[tokio::test]
    async fn test_friend_relay_initialization() {
        let mut config = StealthConfig::default();
        config.friend_nodes = vec![
            FriendNodeConfig {
                peer_id: "friend1".to_string(),
                multiaddr: "/ip4/127.0.0.1/tcp/60001".parse().unwrap(),
                public_key: "pub1".to_string(),
            },
            FriendNodeConfig {
                peer_id: "friend2".to_string(),
                multiaddr: "/ip4/127.0.0.1/tcp/60002".parse().unwrap(),
                public_key: "pub2".to_string(),
            },
        ];

        let waku_provider = MockWakuProvider::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (relay, _handle) = FriendRelay::new(config, waku_provider, shutdown_rx);

        assert_eq!(relay.friends.len(), 2);
        assert!(relay.friends.contains_key("friend1"));
        assert!(relay.friends.contains_key("friend2"));
    }
}