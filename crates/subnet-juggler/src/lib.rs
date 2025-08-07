use anyhow::Result;
use chrono::{DateTime, Utc};
use rand::seq::SliceRandom;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use stealth_common::{EpochInfo, StealthConfig, StealthError, StealthResult, SubnetId};
use tokio::sync::{mpsc, watch};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

pub mod reth_provider;
pub use reth_provider::{RethNetworkProvider, NetworkEvent, NetworkCommand};

/// Commands that can be sent to the SubnetJuggler
#[derive(Debug, Clone)]
pub enum SubnetCommand {
    /// Force an immediate subnet reshuffle
    ForceReshuffle,
    /// Add specific subnets to the current selection
    AddSubnets(Vec<SubnetId>),
    /// Remove specific subnets from the current selection  
    RemoveSubnets(Vec<SubnetId>),
    /// Get current subnet status
    GetStatus,
    /// Stop the subnet juggler
    Stop,
}

/// Events emitted by the SubnetJuggler
#[derive(Debug, Clone)]
pub enum SubnetEvent {
    /// Subnets have been joined
    SubnetsJoined(Vec<SubnetId>),
    /// Subnets have been left
    SubnetsLeft(Vec<SubnetId>),
    /// Epoch boundary reached, reshuffling
    EpochReshuffle { epoch: u64, new_subnets: Vec<SubnetId> },
    /// Error occurred
    Error(String),
}

/// Current subnet subscription state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubnetState {
    pub current_epoch: u64,
    pub subscribed_subnets: HashSet<SubnetId>,
    pub mandatory_subnets: HashSet<SubnetId>, // The validator's required subnets
    pub extra_subnets: HashSet<SubnetId>,     // Additional privacy subnets
    pub last_reshuffle: DateTime<Utc>,
    pub next_reshuffle: DateTime<Utc>,
}

/// Interface to interact with Ethereum networking layer
#[async_trait::async_trait]
pub trait NetworkingProvider: Send + Sync {
    /// Subscribe to an attestation subnet
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()>;
    
    /// Unsubscribe from an attestation subnet
    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()>;
    
    /// Get current epoch information
    async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo>;
    
    /// Get validator's required subnets for current epoch
    async fn get_validator_subnets(&self, validator_pubkey: &str) -> StealthResult<Vec<SubnetId>>;
}

/// Implementation of NetworkingProvider using reth RPC (deprecated - use SystemClockProvider)
pub struct RethProvider {
    client: reqwest::Client,
    base_url: String,
}

impl RethProvider {
    pub fn new(reth_rpc_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: reth_rpc_url,
        }
    }

    async fn make_request<T>(&self, path: &str) -> StealthResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| StealthError::ConsensusApi(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(StealthError::ConsensusApi(format!(
                "HTTP {} for {}",
                response.status(),
                url
            )));
        }

        response
            .json()
            .await
            .map_err(|e| StealthError::ConsensusApi(format!("JSON decode failed: {}", e)))
    }
}

#[async_trait::async_trait]
impl NetworkingProvider for RethProvider {
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        // In practice, this would use reth's libp2p networking
        // For now, we'll simulate the call
        debug!("Subscribing to subnet {}", subnet_id.0);
        
        // POST to /eth/v1/node/network/subscriptions with subnet topic
        let topic = subnet_id.as_topic_name();
        let body = serde_json::json!({
            "topics": [topic]
        });

        let url = format!("{}/eth/v1/node/network/subscriptions", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| StealthError::ConsensusApi(format!("Subscription failed: {}", e)))?;

        if response.status().is_success() {
            info!("Successfully subscribed to subnet {}", subnet_id.0);
            Ok(())
        } else {
            Err(StealthError::ConsensusApi(format!(
                "Failed to subscribe to subnet {}: HTTP {}",
                subnet_id.0,
                response.status()
            )))
        }
    }

    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        debug!("Unsubscribing from subnet {}", subnet_id.0);
        
        let topic = subnet_id.as_topic_name();
        let url = format!("{}/eth/v1/node/network/subscriptions/{}", self.base_url, topic);
        
        let response = self
            .client
            .delete(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| StealthError::ConsensusApi(format!("Unsubscription failed: {}", e)))?;

        if response.status().is_success() {
            info!("Successfully unsubscribed from subnet {}", subnet_id.0);
            Ok(())
        } else {
            Err(StealthError::ConsensusApi(format!(
                "Failed to unsubscribe from subnet {}: HTTP {}",
                subnet_id.0,
                response.status()
            )))
        }
    }

    async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo> {
        #[derive(Deserialize)]
        struct GenesisResponse {
            data: GenesisData,
        }

        #[derive(Deserialize)]
        struct GenesisData {
            genesis_time: String,
        }

        #[derive(Deserialize)]
        struct SyncResponse {
            data: SyncData,
        }

        #[derive(Deserialize)]
        struct SyncData {
            head_slot: String,
        }

        // Get genesis time and current slot
        let genesis_resp: GenesisResponse = self.make_request("/eth/v1/beacon/genesis").await?;
        let sync_resp: SyncResponse = self.make_request("/eth/v1/node/syncing").await?;

        let head_slot: u64 = sync_resp
            .data
            .head_slot
            .parse()
            .map_err(|e| StealthError::ConsensusApi(format!("Invalid slot number: {}", e)))?;

        // Mainnet constants
        const SLOTS_PER_EPOCH: u64 = 32;
        const SECONDS_PER_SLOT: u64 = 12;

        let current_epoch = head_slot / SLOTS_PER_EPOCH;

        Ok(EpochInfo {
            epoch: current_epoch,
            slot: head_slot,
            slots_per_epoch: SLOTS_PER_EPOCH,
            seconds_per_slot: SECONDS_PER_SLOT,
        })
    }

    async fn get_validator_subnets(&self, validator_pubkey: &str) -> StealthResult<Vec<SubnetId>> {
        // This would calculate the validator's assigned subnets based on epoch
        // For demo purposes, we'll return a fixed set
        debug!("Getting validator subnets for {}", validator_pubkey);
        
        // In practice, this would be determined by the validator's index and current epoch
        // For now, return subnets 0 and 1 as the "mandatory" backbone subnets
        Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
    }
}

/// The main SubnetJuggler component that manages dynamic subnet subscriptions
pub struct SubnetJuggler<P: NetworkingProvider> {
    config: StealthConfig,
    provider: P,
    state: SubnetState,
    command_tx: mpsc::UnboundedSender<SubnetCommand>,
    command_rx: mpsc::UnboundedReceiver<SubnetCommand>,
    event_tx: mpsc::UnboundedSender<SubnetEvent>,
    shutdown_rx: watch::Receiver<bool>,
}

impl<P: NetworkingProvider> SubnetJuggler<P> {
    pub fn new(
        config: StealthConfig,
        provider: P,
        shutdown_rx: watch::Receiver<bool>,
    ) -> (Self, SubnetJugglerHandle) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let juggler = Self {
            config,
            provider,
            state: SubnetState {
                current_epoch: 0,
                subscribed_subnets: HashSet::new(),
                mandatory_subnets: HashSet::new(),
                extra_subnets: HashSet::new(),
                last_reshuffle: Utc::now(),
                next_reshuffle: Utc::now(),
            },
            command_tx: command_tx.clone(),
            command_rx,
            event_tx: event_tx.clone(),
            shutdown_rx,
        };

        let handle = SubnetJugglerHandle {
            command_tx,
            event_rx,
        };

        (juggler, handle)
    }

    /// Main event loop for the subnet juggler
    pub async fn run(&mut self) -> StealthResult<()> {
        info!("Starting SubnetJuggler...");

        // Initialize with current epoch info
        self.initialize().await?;

        // Set up epoch boundary timer
        let mut epoch_timer = interval(Duration::from_secs(
            self.state.next_reshuffle.signed_duration_since(Utc::now()).num_seconds() as u64
        ));

        loop {
            tokio::select! {
                // Check for shutdown signal
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("Shutdown signal received, stopping SubnetJuggler");
                        break;
                    }
                }

                // Handle epoch boundary
                _ = epoch_timer.tick() => {
                    if let Err(e) = self.handle_epoch_boundary().await {
                        error!("Error handling epoch boundary: {}", e);
                        let _ = self.event_tx.send(SubnetEvent::Error(e.to_string()));
                    }
                }

                // Handle commands
                Some(command) = self.command_rx.recv() => {
                    if let Err(e) = self.handle_command(command).await {
                        error!("Error handling command: {}", e);
                        let _ = self.event_tx.send(SubnetEvent::Error(e.to_string()));
                    }
                }
            }
        }

        // Clean up subscriptions on shutdown
        self.cleanup().await?;
        Ok(())
    }

    async fn initialize(&mut self) -> StealthResult<()> {
        info!("Initializing SubnetJuggler...");

        // Get current epoch info
        let epoch_info = self.provider.get_current_epoch_info().await?;
        self.state.current_epoch = epoch_info.epoch;

        // Calculate next epoch boundary
        self.state.next_reshuffle = Utc::now() + 
            chrono::Duration::seconds(epoch_info.seconds_until_next_epoch() as i64);

        // Get validator's mandatory subnets (this would come from validator config)
        // For demo, we'll use a hardcoded validator pubkey
        let validator_pubkey = "0x1234567890abcdef"; // TODO: make configurable
        let mandatory_subnets = self.provider.get_validator_subnets(validator_pubkey).await?;
        self.state.mandatory_subnets = mandatory_subnets.into_iter().collect();

        // Subscribe to mandatory subnets
        for subnet in &self.state.mandatory_subnets {
            self.provider.subscribe_to_subnet(*subnet).await?;
            self.state.subscribed_subnets.insert(*subnet);
        }

        // Perform initial reshuffle
        self.reshuffle_extra_subnets().await?;

        Ok(())
    }

    async fn handle_epoch_boundary(&mut self) -> StealthResult<()> {
        info!("Epoch boundary reached, reshuffling subnets...");

        // Update epoch info
        let epoch_info = self.provider.get_current_epoch_info().await?;
        self.state.current_epoch = epoch_info.epoch;

        // Calculate next epoch boundary
        self.state.next_reshuffle = Utc::now() + 
            chrono::Duration::seconds(epoch_info.seconds_until_next_epoch() as i64);

        // Reshuffle extra subnets
        let new_subnets = self.reshuffle_extra_subnets().await?;

        // Emit event
        let _ = self.event_tx.send(SubnetEvent::EpochReshuffle {
            epoch: epoch_info.epoch,
            new_subnets,
        });

        Ok(())
    }

    async fn reshuffle_extra_subnets(&mut self) -> StealthResult<Vec<SubnetId>> {
        info!("Reshuffling extra subnets for epoch {}", self.state.current_epoch);

        // Unsubscribe from old extra subnets
        let old_extra_subnets: Vec<_> = self.state.extra_subnets.iter().cloned().collect();
        for subnet in &old_extra_subnets {
            self.provider.unsubscribe_from_subnet(*subnet).await?;
            self.state.subscribed_subnets.remove(subnet);
        }

        if !old_extra_subnets.is_empty() {
            let _ = self.event_tx.send(SubnetEvent::SubnetsLeft(old_extra_subnets));
        }

        // Select new extra subnets randomly
        let mut all_subnets = SubnetId::all_subnets();
        all_subnets.retain(|s| !self.state.mandatory_subnets.contains(s));

        {
            use rand::seq::SliceRandom;
            let mut rng = thread_rng();
            all_subnets.shuffle(&mut rng);
        }

        let new_extra_subnets: Vec<_> = all_subnets
            .into_iter()
            .take(self.config.extra_subnets_per_epoch)
            .collect();

        // Subscribe to new extra subnets
        for subnet in &new_extra_subnets {
            self.provider.subscribe_to_subnet(*subnet).await?;
            self.state.subscribed_subnets.insert(*subnet);
        }

        self.state.extra_subnets = new_extra_subnets.iter().cloned().collect();
        self.state.last_reshuffle = Utc::now();

        if !new_extra_subnets.is_empty() {
            let _ = self.event_tx.send(SubnetEvent::SubnetsJoined(new_extra_subnets.clone()));
        }

        info!(
            "Subscribed to {} extra subnets: {:?}",
            new_extra_subnets.len(),
            new_extra_subnets.iter().map(|s| s.0).collect::<Vec<_>>()
        );

        Ok(new_extra_subnets)
    }

    async fn handle_command(&mut self, command: SubnetCommand) -> StealthResult<()> {
        match command {
            SubnetCommand::ForceReshuffle => {
                info!("Received force reshuffle command");
                self.reshuffle_extra_subnets().await?;
            }
            SubnetCommand::AddSubnets(subnets) => {
                info!("Adding subnets: {:?}", subnets);
                for subnet in &subnets {
                    if !self.state.subscribed_subnets.contains(subnet) {
                        self.provider.subscribe_to_subnet(*subnet).await?;
                        self.state.subscribed_subnets.insert(*subnet);
                        self.state.extra_subnets.insert(*subnet);
                    }
                }
                let _ = self.event_tx.send(SubnetEvent::SubnetsJoined(subnets));
            }
            SubnetCommand::RemoveSubnets(subnets) => {
                info!("Removing subnets: {:?}", subnets);
                for subnet in &subnets {
                    if self.state.extra_subnets.contains(subnet) {
                        self.provider.unsubscribe_from_subnet(*subnet).await?;
                        self.state.subscribed_subnets.remove(subnet);
                        self.state.extra_subnets.remove(subnet);
                    }
                }
                let _ = self.event_tx.send(SubnetEvent::SubnetsLeft(subnets));
            }
            SubnetCommand::GetStatus => {
                debug!("Current subnet state: {:?}", self.state);
            }
            SubnetCommand::Stop => {
                info!("Received stop command");
                return Err(StealthError::SubnetManagement("Stop requested".to_string()));
            }
        }
        Ok(())
    }

    async fn cleanup(&mut self) -> StealthResult<()> {
        info!("Cleaning up subnet subscriptions...");

        // Unsubscribe from all extra subnets
        let extra_subnets: Vec<_> = self.state.extra_subnets.iter().cloned().collect();
        for subnet in extra_subnets {
            if let Err(e) = self.provider.unsubscribe_from_subnet(subnet).await {
                warn!("Failed to unsubscribe from subnet {}: {}", subnet.0, e);
            }
        }

        // Note: We don't unsubscribe from mandatory subnets as those are needed by the validator

        Ok(())
    }

    pub fn get_state(&self) -> &SubnetState {
        &self.state
    }
}

/// Handle to interact with the SubnetJuggler
pub struct SubnetJugglerHandle {
    command_tx: mpsc::UnboundedSender<SubnetCommand>,
    event_rx: mpsc::UnboundedReceiver<SubnetEvent>,
}

impl SubnetJugglerHandle {
    /// Send a command to the SubnetJuggler
    pub fn send_command(&self, command: SubnetCommand) -> StealthResult<()> {
        self.command_tx
            .send(command)
            .map_err(|_| StealthError::SubnetManagement("Channel closed".to_string()))
    }

    /// Receive events from the SubnetJuggler
    pub async fn recv_event(&mut self) -> Option<SubnetEvent> {
        self.event_rx.recv().await
    }

    /// Force an immediate subnet reshuffle
    pub fn force_reshuffle(&self) -> StealthResult<()> {
        self.send_command(SubnetCommand::ForceReshuffle)
    }

    /// Add specific subnets to the subscription
    pub fn add_subnets(&self, subnets: Vec<SubnetId>) -> StealthResult<()> {
        self.send_command(SubnetCommand::AddSubnets(subnets))
    }

    /// Remove specific subnets from the subscription
    pub fn remove_subnets(&self, subnets: Vec<SubnetId>) -> StealthResult<()> {
        self.send_command(SubnetCommand::RemoveSubnets(subnets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    struct MockProvider {
        subscribed_subnets: std::sync::Mutex<HashSet<SubnetId>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                subscribed_subnets: std::sync::Mutex::new(HashSet::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl NetworkingProvider for MockProvider {
        async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
            self.subscribed_subnets.lock().unwrap().insert(subnet_id);
            Ok(())
        }

        async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
            self.subscribed_subnets.lock().unwrap().remove(&subnet_id);
            Ok(())
        }

        async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo> {
            Ok(EpochInfo {
                epoch: 100,
                slot: 3200,
                slots_per_epoch: 32,
                seconds_per_slot: 12,
            })
        }

        async fn get_validator_subnets(&self, _validator_pubkey: &str) -> StealthResult<Vec<SubnetId>> {
            Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
        }
    }

    #[tokio::test]
    async fn test_subnet_juggler_initialization() {
        let config = StealthConfig::default();
        let provider = MockProvider::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (mut juggler, _handle) = SubnetJuggler::new(config, provider, shutdown_rx);

        // Test initialization
        assert!(juggler.initialize().await.is_ok());
        assert_eq!(juggler.state.current_epoch, 100);
        assert_eq!(juggler.state.mandatory_subnets.len(), 2);
        assert!(juggler.state.extra_subnets.len() > 0);
    }

    #[tokio::test]
    async fn test_subnet_reshuffle() {
        let config = StealthConfig::default();
        let provider = MockProvider::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (mut juggler, _handle) = SubnetJuggler::new(config, provider, shutdown_rx);
        juggler.initialize().await.unwrap();

        let initial_extra_subnets = juggler.state.extra_subnets.clone();
        
        // Force reshuffle
        let new_subnets = juggler.reshuffle_extra_subnets().await.unwrap();
        
        // Should have different subnets after reshuffle
        assert_ne!(initial_extra_subnets, juggler.state.extra_subnets);
        assert_eq!(new_subnets.len(), juggler.config.extra_subnets_per_epoch);
    }
}