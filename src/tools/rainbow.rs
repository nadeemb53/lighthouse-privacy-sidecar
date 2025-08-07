use anyhow::Result;
use clap::{Arg, Command};
use libp2p::{gossipsub::TopicHash, PeerId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tracing::{info, warn};

/// The RAINBOW attack tool that demonstrates the deanonymization vulnerability
pub struct RainbowAttacker {
    /// Statistics for each validator public key
    validator_stats: HashMap<String, ValidatorStats>,
    /// Peer ID to IP address mapping
    peer_to_ip: HashMap<PeerId, IpAddr>,
    /// Current epoch information
    current_epoch: u64,
    /// Attack configuration
    config: AttackConfig,
}

#[derive(Debug, Clone)]
pub struct AttackConfig {
    /// How long to run the attack (in seconds)
    pub duration_seconds: u64,
    /// Confidence threshold for declaring a successful mapping
    pub confidence_threshold: f64,
    /// Subnets to monitor (all 64 by default)
    pub monitored_subnets: Vec<u8>,
}

impl Default for AttackConfig {
    fn default() -> Self {
        Self {
            duration_seconds: 120, // 2 minutes for demo
            confidence_threshold: 0.8, // 80% confidence
            monitored_subnets: (0..64).collect(),
        }
    }
}

/// Statistics tracked for each validator
#[derive(Debug, Clone)]
pub struct ValidatorStats {
    pub public_key: String,
    pub assigned_subnets: HashSet<u8>, // Expected backbone subnets
    pub first_seen_peers: HashMap<u8, Vec<(PeerId, Instant)>>, // subnet -> [(peer, timestamp)]
    pub confidence_score: f64,
    pub mapped_ip: Option<IpAddr>,
}

/// A simulated attestation message observed on the network
#[derive(Debug, Clone)]
pub struct ObservedAttestation {
    pub validator_pubkey: String,
    pub subnet_id: u8,
    pub source_peer: PeerId,
    pub timestamp: Instant,
    pub is_backbone_subnet: bool,
}

/// Results of the RAINBOW attack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackResults {
    pub duration_seconds: u64,
    pub total_attestations_observed: u64,
    pub validators_mapped: Vec<ValidatorMapping>,
    pub success_rate: f64,
    pub confidence_distribution: HashMap<String, u64>, // confidence_range -> count
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorMapping {
    pub validator_pubkey: String,
    pub mapped_ip: String,
    pub confidence_score: f64,
    pub evidence_count: u64,
    pub backbone_subnets: Vec<u8>,
}

impl RainbowAttacker {
    pub fn new(config: AttackConfig) -> Self {
        info!("Initializing RAINBOW attack tool");
        info!("Attack duration: {} seconds", config.duration_seconds);
        info!("Monitoring {} subnets", config.monitored_subnets.len());

        Self {
            validator_stats: HashMap::new(),
            peer_to_ip: HashMap::new(),
            current_epoch: 0,
            config,
        }
    }

    /// Start the RAINBOW attack simulation
    pub async fn run_attack(&mut self) -> Result<AttackResults> {
        info!("🌈 Starting RAINBOW deanonymization attack...");
        
        let start_time = Instant::now();
        let attack_duration = Duration::from_secs(self.config.duration_seconds);
        
        // Simulate joining all attestation subnets
        self.join_all_subnets().await?;
        
        // Run the attack for the specified duration
        let mut observation_count = 0u64;
        let mut interval = interval(Duration::from_millis(100)); // Check every 100ms
        
        while start_time.elapsed() < attack_duration {
            interval.tick().await;
            
            // Simulate observing attestations
            let observations = self.simulate_attestation_observations().await;
            observation_count += observations.len() as u64;
            
            // Process each observation
            for observation in observations {
                self.process_attestation_observation(observation).await;
            }
            
            // Print progress every 10 seconds
            if start_time.elapsed().as_secs() % 10 == 0 {
                let progress = (start_time.elapsed().as_secs() * 100) / self.config.duration_seconds;
                info!("Attack progress: {}% ({} attestations observed)", progress, observation_count);
            }
        }
        
        info!("🌈 RAINBOW attack completed in {:.1} seconds", start_time.elapsed().as_secs_f64());
        
        // Analyze results
        let results = self.analyze_results(start_time.elapsed(), observation_count).await;
        self.print_results(&results).await;
        
        Ok(results)
    }

    async fn join_all_subnets(&self) -> Result<()> {
        info!("Joining all {} attestation subnets...", self.config.monitored_subnets.len());
        
        // In a real attack, this would involve:
        // 1. Connecting to the Ethereum P2P network
        // 2. Subscribing to all 64 attestation gossipsub topics
        // 3. Setting up message handlers for each topic
        
        for subnet_id in &self.config.monitored_subnets {
            let topic = format!("/eth2/mainnet/beacon_attestation_{}/ssz_snappy", subnet_id);
            // Simulate subscription
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        
        info!("✓ Successfully joined all attestation subnets");
        Ok(())
    }

    async fn simulate_attestation_observations(&self) -> Vec<ObservedAttestation> {
        use rand::seq::SliceRandom;
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut observations = Vec::new();
        
        // Simulate 1-3 attestations per 100ms window
        let count = rng.gen_range(1..=3);
        
        for _ in 0..count {
            let validator_pubkey = format!("validator_{}", rng.gen_range(0..10)); // 10 validators for demo
            let subnet_id = *self.config.monitored_subnets.choose(&mut rng).unwrap();
            let source_peer = PeerId::random();
            
            // Determine if this is a backbone subnet for this validator
            let validator_index = validator_pubkey.chars()
                .last()
                .unwrap()
                .to_digit(10)
                .unwrap() as u8;
            
            // Each validator has 2 backbone subnets based on their index and current epoch
            let backbone_subnet_1 = (validator_index * 2 + (self.current_epoch % 32) as u8) % 64;
            let backbone_subnet_2 = (validator_index * 2 + 1 + (self.current_epoch % 32) as u8) % 64;
            let is_backbone_subnet = subnet_id == backbone_subnet_1 || subnet_id == backbone_subnet_2;
            
            observations.push(ObservedAttestation {
                validator_pubkey,
                subnet_id,
                source_peer,
                timestamp: Instant::now(),
                is_backbone_subnet,
            });
            
            // Simulate IP address mapping (in reality this would come from peer discovery)
            if !self.peer_to_ip.contains_key(&source_peer) {
                // Generate a realistic IP address
                let ip = format!("192.168.{}.{}", rng.gen_range(1..255), rng.gen_range(1..255))
                    .parse::<IpAddr>()
                    .unwrap();
            }
        }
        
        observations
    }

    async fn process_attestation_observation(&mut self, observation: ObservedAttestation) {
        // This is the core of the RAINBOW attack logic
        
        // Get or create validator stats
        let validator_stats = self.validator_stats
            .entry(observation.validator_pubkey.clone())
            .or_insert_with(|| ValidatorStats {
                public_key: observation.validator_pubkey.clone(),
                assigned_subnets: HashSet::new(),
                first_seen_peers: HashMap::new(),
                confidence_score: 0.0,
                mapped_ip: None,
            });
        
        // Record the backbone subnets for this validator
        if observation.is_backbone_subnet {
            validator_stats.assigned_subnets.insert(observation.subnet_id);
        }
        
        // Track first-seen peers for each subnet
        let subnet_peers = validator_stats.first_seen_peers
            .entry(observation.subnet_id)
            .or_insert_with(Vec::new);
        
        // The key insight: if this is NOT a backbone subnet, the first peer
        // to forward the attestation is likely the validator itself
        if !observation.is_backbone_subnet {
            if subnet_peers.is_empty() {
                // This is the first time we see this validator's attestation on this subnet
                // This peer is highly likely to be the validator
                subnet_peers.push((observation.source_peer, observation.timestamp));
                
                // Increase confidence for this peer
                self.update_confidence_score(
                    &observation.validator_pubkey,
                    observation.source_peer,
                ).await;
            }
        }
    }

    async fn update_confidence_score(&mut self, validator_pubkey: &str, peer: PeerId) {
        if let Some(stats) = self.validator_stats.get_mut(validator_pubkey) {
            // Count how many non-backbone subnets this peer was first on
            let first_seen_count = stats.first_seen_peers
                .iter()
                .filter(|(subnet_id, peers)| {
                    !stats.assigned_subnets.contains(subnet_id) && 
                    peers.first().map(|(p, _)| p) == Some(&peer)
                })
                .count();
            
            // Calculate confidence based on the number of "first seen" events
            // The more non-backbone subnets where this peer appears first, 
            // the higher the confidence that this peer is the validator
            stats.confidence_score = (first_seen_count as f64) / 10.0; // Normalize to 0-1
            
            // If confidence is high enough, map the IP
            if stats.confidence_score >= self.config.confidence_threshold {
                if let Some(ip) = self.peer_to_ip.get(&peer) {
                    stats.mapped_ip = Some(*ip);
                }
            }
        }
    }

    async fn analyze_results(&self, duration: Duration, total_observations: u64) -> AttackResults {
        let mapped_validators: Vec<ValidatorMapping> = self.validator_stats
            .values()
            .filter_map(|stats| {
                stats.mapped_ip.map(|ip| ValidatorMapping {
                    validator_pubkey: stats.public_key.clone(),
                    mapped_ip: ip.to_string(),
                    confidence_score: stats.confidence_score,
                    evidence_count: stats.first_seen_peers.len() as u64,
                    backbone_subnets: stats.assigned_subnets.iter().cloned().collect(),
                })
            })
            .collect();

        let success_rate = if self.validator_stats.is_empty() {
            0.0
        } else {
            (mapped_validators.len() as f64) / (self.validator_stats.len() as f64)
        };

        // Create confidence distribution
        let mut confidence_distribution = HashMap::new();
        for stats in self.validator_stats.values() {
            let bucket = format!("{:.1}-{:.1}", 
                                (stats.confidence_score * 10.0).floor() / 10.0,
                                (stats.confidence_score * 10.0).floor() / 10.0 + 0.1);
            *confidence_distribution.entry(bucket).or_insert(0) += 1;
        }

        AttackResults {
            duration_seconds: duration.as_secs(),
            total_attestations_observed: total_observations,
            validators_mapped: mapped_validators,
            success_rate,
            confidence_distribution,
        }
    }

    async fn print_results(&self, results: &AttackResults) {
        println!("\n🌈 RAINBOW Attack Results");
        println!("========================");
        println!("Duration: {} seconds", results.duration_seconds);
        println!("Attestations observed: {}", results.total_attestations_observed);
        println!("Validators analyzed: {}", self.validator_stats.len());
        println!("Validators successfully mapped: {}", results.validators_mapped.len());
        println!("Success rate: {:.1}%", results.success_rate * 100.0);
        
        if !results.validators_mapped.is_empty() {
            println!("\nSuccessful mappings:");
            for mapping in &results.validators_mapped {
                println!("  {} -> {} (confidence: {:.1}%)", 
                        mapping.validator_pubkey,
                        mapping.mapped_ip,
                        mapping.confidence_score * 100.0);
            }
        }
        
        println!("\nConfidence distribution:");
        for (range, count) in &results.confidence_distribution {
            println!("  {}: {} validators", range, count);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let matches = Command::new("rainbow-attack-tool")
        .version("1.0.0")
        .about("RAINBOW deanonymization attack demonstration tool")
        .arg(
            Arg::new("duration")
                .short('d')
                .long("duration")
                .value_name("SECONDS")
                .help("Attack duration in seconds")
                .default_value("120")
                .value_parser(clap::value_parser!(u64))
        )
        .arg(
            Arg::new("confidence")
                .short('c')
                .long("confidence")
                .value_name("THRESHOLD")
                .help("Confidence threshold for successful mapping (0.0-1.0)")
                .default_value("0.8")
                .value_parser(clap::value_parser!(f64))
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("FILE")
                .help("Output file for results (JSON format)")
        )
        .get_matches();

    let config = AttackConfig {
        duration_seconds: *matches.get_one::<u64>("duration").unwrap(),
        confidence_threshold: *matches.get_one::<f64>("confidence").unwrap(),
        monitored_subnets: (0..64).collect(),
    };

    let mut attacker = RainbowAttacker::new(config);
    let results = attacker.run_attack().await?;

    // Save results if output file specified
    if let Some(output_file) = matches.get_one::<String>("output") {
        let json = serde_json::to_string_pretty(&results)?;
        tokio::fs::write(output_file, json).await?;
        println!("Results saved to: {}", output_file);
    }

    if results.success_rate > 0.0 {
        println!("\n⚠️  VULNERABILITY DEMONSTRATED");
        println!("The RAINBOW attack successfully mapped {:.1}% of validators to IP addresses.", 
                results.success_rate * 100.0);
        println!("This shows why the stealth sidecar defense is necessary!");
    } else {
        println!("\n✅ NO MAPPINGS FOUND");
        println!("The defense appears to be working - no validators were successfully mapped.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attack_config_default() {
        let config = AttackConfig::default();
        assert_eq!(config.duration_seconds, 120);
        assert_eq!(config.confidence_threshold, 0.8);
        assert_eq!(config.monitored_subnets.len(), 64);
    }

    #[tokio::test]
    async fn test_rainbow_attacker_creation() {
        let config = AttackConfig::default();
        let attacker = RainbowAttacker::new(config);
        
        assert_eq!(attacker.validator_stats.len(), 0);
        assert_eq!(attacker.current_epoch, 0);
    }

    #[tokio::test]
    async fn test_confidence_calculation() {
        let config = AttackConfig::default();
        let mut attacker = RainbowAttacker::new(config);
        
        // Simulate an observation on a non-backbone subnet
        let observation = ObservedAttestation {
            validator_pubkey: "test_validator".to_string(),
            subnet_id: 42,
            source_peer: PeerId::random(),
            timestamp: Instant::now(),
            is_backbone_subnet: false,
        };
        
        attacker.process_attestation_observation(observation).await;
        
        let stats = attacker.validator_stats.get("test_validator").unwrap();
        assert!(stats.confidence_score > 0.0);
    }
}