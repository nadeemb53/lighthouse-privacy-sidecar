use alloy_primitives::B256;
use chrono::{DateTime, Utc};
use multiaddr::Multiaddr;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

/// Common error types used throughout the stealth sidecar
#[derive(Error, Debug)]
pub enum StealthError {
    #[error("Network error: {0}")]
    Network(String),
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Lighthouse API error: {0}")]
    LighthouseApi(String),
    #[error("Waku RLN error: {0}")]
    WakuRln(String),
    #[error("Subnet management error: {0}")]
    SubnetManagement(String),
    #[error("Metrics error: {0}")]
    Metrics(String),
}

/// Ethereum epoch and slot information
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochInfo {
    pub epoch: u64,
    pub slot: u64,
    pub slots_per_epoch: u64,
    pub seconds_per_slot: u64,
}

impl EpochInfo {
    /// Calculate the current epoch from a slot
    pub fn epoch_from_slot(slot: u64, slots_per_epoch: u64) -> u64 {
        slot / slots_per_epoch
    }

    /// Calculate slots remaining in current epoch
    pub fn slots_remaining_in_epoch(&self) -> u64 {
        self.slots_per_epoch - (self.slot % self.slots_per_epoch)
    }

    /// Calculate time until next epoch
    pub fn seconds_until_next_epoch(&self) -> u64 {
        self.slots_remaining_in_epoch() * self.seconds_per_slot
    }
}

/// Attestation subnet identifier (0-63 for mainnet)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubnetId(pub u8);

impl SubnetId {
    pub const MAX_SUBNET_ID: u8 = 63;
    
    pub fn new(id: u8) -> Result<Self, StealthError> {
        if id > Self::MAX_SUBNET_ID {
            return Err(StealthError::Config(format!(
                "Invalid subnet ID: {}. Must be 0-{}",
                id, Self::MAX_SUBNET_ID
            )));
        }
        Ok(SubnetId(id))
    }

    pub fn all_subnets() -> Vec<SubnetId> {
        (0..=Self::MAX_SUBNET_ID).map(SubnetId).collect()
    }

    pub fn as_topic_name(&self) -> String {
        format!("/eth2/{}/beacon_attestation_{}/ssz_snappy", 
                 "mainnet", // TODO: make configurable for different networks
                 self.0)
    }
}

/// Validator public key and associated metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub pubkey: String, // Use hex string instead of B256 for simpler serialization
    pub validator_index: u64,
    pub assigned_subnets: Vec<SubnetId>,
    pub last_attestation_slot: Option<u64>,
}

/// Configuration for the stealth sidecar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StealthConfig {
    /// Lighthouse beacon node HTTP API endpoint
    pub lighthouse_http_api: String,
    
    /// Number of extra subnets to join per epoch (6-10 recommended)
    pub extra_subnets_per_epoch: usize,
    
    /// Friend nodes for the privacy mesh
    pub friend_nodes: Vec<FriendNodeConfig>,
    
    /// Waku node configuration
    pub waku_config: WakuConfig,
    
    /// Prometheus metrics configuration
    pub metrics: MetricsConfig,
    
    /// Network configuration
    pub network: NetworkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendNodeConfig {
    pub peer_id: String,
    pub multiaddr: Multiaddr,
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakuConfig {
    /// Local nwaku node RPC endpoint
    pub nwaku_rpc_url: String,
    
    /// RLN contract address
    pub rln_contract_address: Option<String>,
    
    /// Rate limit (messages per epoch)
    pub rate_limit_per_epoch: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub listen_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub listen_port: u16,
    pub external_ip: Option<String>,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            lighthouse_http_api: "http://localhost:5052".to_string(),
            extra_subnets_per_epoch: 8,
            friend_nodes: Vec::new(),
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
        }
    }
}

/// Metrics tracked by the stealth sidecar
#[derive(Debug, Clone)]
pub struct StealthMetrics {
    pub attestations_relayed: u64,
    pub subnets_joined_total: u64,
    pub subnets_left_total: u64,
    pub friend_messages_sent: u64,
    pub friend_messages_received: u64,
    pub average_latency_ms: f64,
    pub bandwidth_bytes_per_second: f64,
    pub timestamp: DateTime<Utc>,
}

impl Default for StealthMetrics {
    fn default() -> Self {
        Self {
            attestations_relayed: 0,
            subnets_joined_total: 0,
            subnets_left_total: 0,
            friend_messages_sent: 0,
            friend_messages_received: 0,
            average_latency_ms: 0.0,
            bandwidth_bytes_per_second: 0.0,
            timestamp: Utc::now(),
        }
    }
}

/// Result type alias for stealth operations
pub type StealthResult<T> = Result<T, StealthError>;

/// Helper functions for common operations
pub mod utils {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::thread_rng;

    /// Generate a random selection of subnet IDs
    pub fn random_subnet_selection(count: usize) -> Vec<SubnetId> {
        let mut all_subnets = SubnetId::all_subnets();
        let mut rng = thread_rng();
        all_subnets.shuffle(&mut rng);
        all_subnets.into_iter().take(count).collect()
    }

    /// Calculate epoch boundary timing
    pub fn next_epoch_boundary(current_epoch_info: &EpochInfo) -> DateTime<Utc> {
        let seconds_until_next = current_epoch_info.seconds_until_next_epoch();
        Utc::now() + chrono::Duration::seconds(seconds_until_next as i64)
    }

    /// Validate configuration
    pub fn validate_config(config: &StealthConfig) -> StealthResult<()> {
        if config.extra_subnets_per_epoch > 32 {
            return Err(StealthError::Config(
                "Too many extra subnets per epoch. Maximum is 32.".to_string()
            ));
        }

        if config.friend_nodes.len() < 3 {
            return Err(StealthError::Config(
                "At least 3 friend nodes required for k-anonymity.".to_string()
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subnet_id_creation() {
        assert!(SubnetId::new(0).is_ok());
        assert!(SubnetId::new(63).is_ok());
        assert!(SubnetId::new(64).is_err());
    }

    #[test]
    fn test_epoch_calculations() {
        let epoch_info = EpochInfo {
            epoch: 10,
            slot: 320, // 10 * 32 + 0
            slots_per_epoch: 32,
            seconds_per_slot: 12,
        };

        assert_eq!(epoch_info.slots_remaining_in_epoch(), 32);
        assert_eq!(epoch_info.seconds_until_next_epoch(), 384);
    }

    #[test]
    fn test_random_subnet_selection() {
        let subnets = utils::random_subnet_selection(8);
        assert_eq!(subnets.len(), 8);
        
        // Ensure all are unique
        let mut unique_subnets = subnets.clone();
        unique_subnets.sort_by_key(|s| s.0);
        unique_subnets.dedup();
        assert_eq!(unique_subnets.len(), 8);
    }
}