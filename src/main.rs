use anyhow::Result;
use clap::Parser;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;
use tracing::{info, warn, error, debug};

// Import our privacy sidecar components
use subnet_juggler::{SubnetJuggler, SubnetJugglerHandle, SubnetCommand, BeaconNetworkProvider};
use friend_relay::{FriendRelay, NwakuProvider, RelayCommand};
use stealth_common::StealthConfig;
use stealth_metrics::{StealthMetricsCollector, MetricsServer, start_system_metrics_updater};

/// Main lighthouse-privacy-sidecar binary
#[derive(Parser, Debug)]
#[command(name = "lighthouse-privacy-sidecar")]
#[command(about = "Privacy-enhancing sidecar for Lighthouse validators")]
#[command(version = "1.0.0")]
struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "config/stealth-sidecar.toml")]
    config: PathBuf,

    /// Enable stealth mode on startup
    #[arg(long)]
    stealth: bool,

    /// Bootstrap peers for libp2p (real mainnet beacon nodes)
    #[arg(long, value_delimiter = ' ', default_values = [
        "/ip4/4.157.240.54/tcp/9000/p2p/16Uiu2HAm5a1z45GYvdBZgGh8b5jB6jm1YcgP5TdhqfqmpVsM6gFV",
        "/ip4/4.196.214.4/tcp/9000/p2p/16Uiu2HAm5CQgaLeFXLFpn7YbYfKXGTGgJBP1vKKg5gLJKPKe2VKb"
    ])]
    bootstrap_peers: Vec<String>,

    /// Validator public key for subnet calculation
    #[arg(long)]
    validator_pubkey: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug)]
enum SidecarCommand {
    EnableStealth,
    DisableStealth,
    GetStatus,
    SendAttestation { validator_id: u8, subnet_id: u8, data: String },
    Shutdown,
}

/// Main sidecar state management
struct PrivacySidecar {
    config: StealthConfig,
    stealth_enabled: bool,
    
    // Real privacy components
    subnet_juggler_handle: Option<SubnetJugglerHandle>,
    friend_relay_handle: Option<friend_relay::FriendRelayHandle>,
    
    // Metrics collection
    metrics_collector: Option<Arc<StealthMetricsCollector>>,
    
    // Beacon network provider
    beacon_network: Option<BeaconNetworkProvider>,
}

impl PrivacySidecar {
    async fn new(config: StealthConfig) -> Result<Self> {
        // Initialize metrics if enabled
        let metrics_collector = if config.metrics.enabled {
            match StealthMetricsCollector::new() {
                Ok(collector) => {
                    let collector_arc = Arc::new(collector);
                    
                    // Start metrics server
                    let metrics_server = MetricsServer::new(
                        collector_arc.clone(), 
                        config.metrics.clone()
                    );
                    tokio::spawn(async move {
                        if let Err(e) = metrics_server.start().await {
                            error!("Metrics server error: {}", e);
                        }
                    });
                    
                    // Start system metrics updater
                    tokio::spawn(start_system_metrics_updater(collector_arc.clone()));
                    
                    info!("📊 Metrics server started on http://{}:{}/metrics", 
                        config.metrics.listen_address, 
                        config.metrics.listen_port);
                    
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
            config,
            stealth_enabled: false,
            subnet_juggler_handle: None,
            friend_relay_handle: None,
            metrics_collector,
            beacon_network: None,
        })
    }

    async fn enable_stealth(&mut self, bootstrap_peers: Vec<String>) -> Result<()> {
        if self.stealth_enabled {
            info!("🛡️  Stealth mode already enabled");
            return Ok(());
        }

        info!("🛡️  ENABLING STEALTH MODE");
        self.stealth_enabled = true;
        
        // Initialize real beacon network provider
        let beacon_network = BeaconNetworkProvider::new(bootstrap_peers).await
            .map_err(|e| anyhow::anyhow!("Failed to initialize beacon network: {}", e))?;
        
        // Start real subnet juggler with beacon network provider
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        
        let (mut subnet_juggler, handle) = SubnetJuggler::new(
            self.config.clone(),
            beacon_network,
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
        let waku_provider = NwakuProvider::new(&self.config.waku_config);
        let (shutdown_tx2, shutdown_rx2) = watch::channel(false);
            
        let (mut friend_relay, friend_relay_handle) = FriendRelay::new(
            self.config.clone(),
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
            for _ in 0..self.config.extra_subnets_per_epoch {
                metrics.record_subnet_joined(false); // Extra subnets
            }
            metrics.privacy_events_total.with_label_values(&["subnet_shuffle"]).inc();
        }

        info!("✅ Subnet juggler started - will shuffle {} extra subnets per epoch", 
            self.config.extra_subnets_per_epoch);
        info!("✅ Friend relay mesh activated ({} trusted nodes)", 
            self.config.friend_nodes.len());
        info!("✅ Privacy protection enabled");
        
        Ok(())
    }

    async fn disable_stealth(&mut self) -> Result<()> {
        if !self.stealth_enabled {
            info!("🔓 Stealth mode already disabled");
            return Ok(());
        }

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
        
        info!("✅ Privacy protection disabled");
        Ok(())
    }

    async fn get_status(&self) -> Value {
        let peer_count = if let Some(network) = &self.beacon_network {
            network.get_peer_count().await
        } else {
            0
        };

        // Calculate current epoch using Ethereum constants
        let genesis_time = 1606824023u64; // Ethereum mainnet genesis
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        let current_slot = if now > genesis_time {
            (now - genesis_time) / 12
        } else {
            0
        };
        let current_epoch = current_slot / 32;

        json!({
            "stealth_enabled": self.stealth_enabled,
            "current_epoch": current_epoch,
            "current_slot": current_slot,
            "extra_subnets_per_epoch": self.config.extra_subnets_per_epoch,
            "friend_nodes_count": self.config.friend_nodes.len(),
            "metrics_enabled": self.config.metrics.enabled,
            "peer_count": peer_count,
            "networking": "beacon_chain_gossipsub"
        })
    }

    async fn handle_send_attestation(&mut self, validator_id: u8, subnet_id: u8, data: &str) -> Result<()> {
        info!("📝 Attestation from validator {} on subnet {}", validator_id, subnet_id);
        
        if self.stealth_enabled {
            info!("🛡️  Protected: Forwarding through privacy mesh + subnet shuffling");
            
            // Use real friend relay to forward the attestation
            if let Some(friend_relay) = &self.friend_relay_handle {
                let start_time = Instant::now();
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
        
        Ok(())
    }
}

async fn load_config(config_path: &PathBuf) -> Result<StealthConfig> {
    if config_path.exists() {
        let contents = tokio::fs::read_to_string(config_path).await?;
        let config: StealthConfig = toml::from_str(&contents)?;
        info!("📋 Loaded configuration from {}", config_path.display());
        Ok(config)
    } else {
        warn!("Configuration file not found at {}, using defaults", config_path.display());
        Ok(StealthConfig::default())
    }
}

async fn run_sidecar(args: Args) -> Result<()> {
    info!("🚀 Starting lighthouse-privacy-sidecar");
    info!("   Config: {}", args.config.display());
    info!("   Bootstrap peers: {:?}", args.bootstrap_peers);
    
    // Load configuration
    let config = load_config(&args.config).await?;
    
    // Use bootstrap peers from config if not provided via CLI
    let bootstrap_peers = if args.bootstrap_peers.is_empty() {
        config.network.bootstrap_peers.clone().unwrap_or_default()
    } else {
        args.bootstrap_peers.clone()
    };
    
    // Initialize sidecar
    let mut sidecar = PrivacySidecar::new(config).await?;
    
    // Enable stealth if requested
    if args.stealth {
        sidecar.enable_stealth(bootstrap_peers.clone()).await?;
    }
    
    info!("✅ Sidecar started successfully");
    if sidecar.metrics_collector.is_some() {
        info!("📊 Metrics available at: http://{}:{}/metrics", 
            sidecar.config.metrics.listen_address, 
            sidecar.config.metrics.listen_port);
    }
    
    // Simple demonstration loop
    info!("🎯 Privacy sidecar ready for Lighthouse integration");
    info!("   Apply patch: lighthouse-patch/attestation_service.patch");
    info!("   Privacy features: {} extra subnets, {} friend nodes", 
        sidecar.config.extra_subnets_per_epoch,
        sidecar.config.friend_nodes.len());
    
    // Run indefinitely until Ctrl+C
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("🛑 Shutdown signal received");
                break;
            }
            
            // Heartbeat every 30 seconds
            _ = sleep(Duration::from_secs(30)) => {
                let status = sidecar.get_status().await;
                debug!("💓 Privacy sidecar active - epoch: {}, peers: {}", 
                    status["current_epoch"], status["peer_count"]);
            }
        }
    }
    
    // Cleanup
    if sidecar.stealth_enabled {
        sidecar.disable_stealth().await?;
    }
    
    info!("👋 lighthouse-privacy-sidecar shut down cleanly");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize tracing
    let filter = if args.verbose {
        "lighthouse_privacy_sidecar=debug,subnet_juggler=debug,friend_relay=debug,stealth_metrics=debug"
    } else {
        "lighthouse_privacy_sidecar=info,subnet_juggler=info,friend_relay=info,stealth_metrics=info"
    };
    
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    if let Err(e) = run_sidecar(args).await {
        error!("Sidecar failed: {}", e);
        std::process::exit(1);
    }
    
    Ok(())
}