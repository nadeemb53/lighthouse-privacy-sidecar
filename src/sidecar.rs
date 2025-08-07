use anyhow::Result;
use friend_relay::{FriendRelay, FriendRelayHandle, NwakuProvider, RelayEvent};
use std::sync::Arc;
use stealth_common::{StealthConfig, StealthError, StealthResult};
use stealth_metrics::StealthMetricsCollector;
use subnet_juggler::{LighthouseProvider, SubnetJuggler, SubnetJugglerHandle, SubnetEvent};
use tokio::sync::watch;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

use crate::reth_integration::RethGossipInterceptor;

/// The main stealth sidecar orchestrating subnet juggling and friend relay
pub struct StealthSidecar {
    config: StealthConfig,
    metrics: Arc<StealthMetricsCollector>,
    
    // Component handles
    subnet_juggler_handle: SubnetJugglerHandle,
    friend_relay_handle: FriendRelayHandle,
    reth_interceptor: RethGossipInterceptor,
    
    shutdown_rx: watch::Receiver<bool>,
}

impl StealthSidecar {
    pub async fn new(
        config: StealthConfig,
        metrics: Arc<StealthMetricsCollector>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> StealthResult<Self> {
        info!("Initializing reth-stealth-sidecar components...");

        // Initialize Lighthouse provider for subnet management
        let lighthouse_provider = LighthouseProvider::new(config.lighthouse_http_api.clone());

        // Initialize Waku provider for friend mesh
        let waku_provider = NwakuProvider::new(&config.waku_config);

        // Create subnet juggler
        let (mut subnet_juggler, subnet_juggler_handle) = 
            SubnetJuggler::new(config.clone(), lighthouse_provider, shutdown_rx.clone());

        // Create friend relay
        let (mut friend_relay, friend_relay_handle) = 
            FriendRelay::new(config.clone(), waku_provider, shutdown_rx.clone());

        // Initialize reth gossip interceptor
        let reth_interceptor = RethGossipInterceptor::new(config.clone()).await?;

        // Spawn background tasks
        let metrics_clone = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = subnet_juggler.run().await {
                error!("SubnetJuggler error: {}", e);
                metrics_clone.record_privacy_event_error("subnet_juggler");
            }
        });

        let metrics_clone = metrics.clone();
        tokio::spawn(async move {
            if let Err(e) = friend_relay.run().await {
                error!("FriendRelay error: {}", e);
                metrics_clone.record_privacy_event_error("friend_relay");
            }
        });

        info!("reth-stealth-sidecar initialization complete");

        Ok(Self {
            config,
            metrics,
            subnet_juggler_handle,
            friend_relay_handle,
            reth_interceptor,
            shutdown_rx,
        })
    }

    /// Main event loop for the stealth sidecar
    pub async fn run(&mut self) -> StealthResult<()> {
        info!("Starting reth-stealth-sidecar main event loop");

        // Start the reth gossip interceptor
        let mut gossip_stream = self.reth_interceptor.start_interception().await?;

        // Set up periodic health checks
        let mut health_check_interval = interval(Duration::from_secs(30));

        // Event processing loop
        loop {
            tokio::select! {
                // Check for shutdown signal
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("Shutdown signal received in main loop");
                        break;
                    }
                }

                // Handle gossip messages from reth
                Some(gossip_msg) = gossip_stream.recv() => {
                    if let Err(e) = self.handle_gossip_message(gossip_msg).await {
                        warn!("Error handling gossip message: {}", e);
                    }
                }

                // Handle subnet juggler events
                Some(subnet_event) = self.subnet_juggler_handle.recv_event() => {
                    if let Err(e) = self.handle_subnet_event(subnet_event).await {
                        warn!("Error handling subnet event: {}", e);
                    }
                }

                // Handle friend relay events
                Some(relay_event) = self.friend_relay_handle.recv_event() => {
                    if let Err(e) = self.handle_relay_event(relay_event).await {
                        warn!("Error handling relay event: {}", e);
                    }
                }

                // Periodic health checks
                _ = health_check_interval.tick() => {
                    self.perform_health_check().await;
                }
            }
        }

        info!("reth-stealth-sidecar main loop shutting down");
        Ok(())
    }

    async fn handle_gossip_message(
        &self,
        message: crate::reth_integration::GossipMessage,
    ) -> StealthResult<()> {
        use crate::reth_integration::GossipMessage;

        match message {
            GossipMessage::Attestation { data, subnet_id, .. } => {
                debug!("Intercepted attestation on subnet {}", subnet_id);
                
                // Record metrics
                self.metrics.record_message_size(data.len(), "attestation");
                self.metrics.record_bandwidth(data.len() as u64, "inbound", "gossip");

                // Relay through friend mesh for privacy
                if let Err(e) = self.friend_relay_handle.relay_attestation(data, subnet_id) {
                    warn!("Failed to relay attestation through friends: {}", e);
                } else {
                    debug!("Attestation successfully queued for friend relay");
                }
            }
            GossipMessage::Block { data, .. } => {
                debug!("Intercepted block (size: {} bytes)", data.len());
                self.metrics.record_message_size(data.len(), "block");
                self.metrics.record_bandwidth(data.len() as u64, "inbound", "gossip");
            }
            GossipMessage::Other { topic, data, .. } => {
                debug!("Intercepted other gossip message on topic: {}", topic);
                self.metrics.record_message_size(data.len(), "other");
                self.metrics.record_bandwidth(data.len() as u64, "inbound", "gossip");
            }
        }

        Ok(())
    }

    async fn handle_subnet_event(&self, event: SubnetEvent) -> StealthResult<()> {
        match event {
            SubnetEvent::SubnetsJoined(subnets) => {
                info!("Joined {} extra subnets: {:?}", 
                      subnets.len(), 
                      subnets.iter().map(|s| s.0).collect::<Vec<_>>());
                
                for subnet in subnets {
                    self.metrics.record_subnet_joined(false); // false = extra subnet
                    
                    // Notify reth to subscribe to this subnet's gossip topic
                    if let Err(e) = self.reth_interceptor.subscribe_to_subnet(subnet).await {
                        warn!("Failed to subscribe reth to subnet {}: {}", subnet.0, e);
                    }
                }
                
                self.metrics.privacy_events_total
                    .with_label_values(&["subnet_shuffle"])
                    .inc();
            }
            SubnetEvent::SubnetsLeft(subnets) => {
                info!("Left {} subnets: {:?}", 
                      subnets.len(), 
                      subnets.iter().map(|s| s.0).collect::<Vec<_>>());
                
                for subnet in subnets {
                    self.metrics.record_subnet_left(false);
                    
                    // Notify reth to unsubscribe from this subnet's gossip topic
                    if let Err(e) = self.reth_interceptor.unsubscribe_from_subnet(subnet).await {
                        warn!("Failed to unsubscribe reth from subnet {}: {}", subnet.0, e);
                    }
                }
            }
            SubnetEvent::EpochReshuffle { epoch, new_subnets } => {
                info!("Epoch {} reshuffle complete: {} new subnets", epoch, new_subnets.len());
                self.metrics.record_epoch_reshuffle(1.0); // TODO: measure actual duration
                
                // Update current subnet count
                self.metrics.current_subscribed_subnets.set(new_subnets.len() as i64);
            }
            SubnetEvent::Error(err) => {
                error!("Subnet juggler error: {}", err);
                self.metrics.privacy_events_total
                    .with_label_values(&["error"])
                    .inc();
            }
        }

        Ok(())
    }

    async fn handle_relay_event(&self, event: RelayEvent) -> StealthResult<()> {
        match event {
            RelayEvent::MessageRelayed { message_id, friends_count, latency_ms } => {
                info!("Relayed message {} to {} friends in {}ms", 
                      message_id, friends_count, latency_ms);
                
                self.metrics.record_attestation_relayed(latency_ms as f64 / 1000.0);
                self.metrics.update_peer_connections("friend", friends_count as i64);
                
                // Record anonymity preservation
                self.metrics.privacy_events_total
                    .with_label_values(&["anonymity_preserved"])
                    .inc();
            }
            RelayEvent::MessageReceived { message_id, from_friend } => {
                debug!("Received message {} from friend {}", message_id, from_friend);
                self.metrics.attestations_received_total.inc();
                self.metrics.record_friend_message_received(&from_friend);
            }
            RelayEvent::FriendConnected(friend_id) => {
                info!("Friend {} connected", friend_id);
                // Update connection metrics would go here
            }
            RelayEvent::FriendDisconnected(friend_id) => {
                warn!("Friend {} disconnected", friend_id);
                // Update connection metrics would go here
            }
            RelayEvent::RateLimitExceeded { epoch, attempts, limit } => {
                warn!("RLN rate limit exceeded in epoch {}: {}/{}", epoch, attempts, limit);
                self.metrics.record_rate_limit_violation();
            }
            RelayEvent::Error(err) => {
                error!("Friend relay error: {}", err);
                self.metrics.privacy_events_total
                    .with_label_values(&["error"])
                    .inc();
            }
        }

        Ok(())
    }

    async fn perform_health_check(&self) {
        debug!("Performing health check...");
        
        // Update system metrics
        self.metrics.update_system_metrics();
        
        // Check if we have enough friends connected
        if self.config.friend_nodes.len() < 3 {
            warn!("Less than 3 friends configured - k-anonymity may be compromised");
        }

        // Log current status
        let stats = self.metrics.get_dashboard_stats();
        debug!("Health check - Uptime: {}s, Attestations relayed: {}, Friends: {}", 
               stats.uptime_seconds, stats.attestations_relayed, stats.friends_connected);
    }
}

// Extension trait for metrics to add helper methods
trait MetricsExt {
    fn record_privacy_event_error(&self, component: &str);
    fn record_friend_message_received(&self, friend_id: &str);
    fn get_dashboard_stats(&self) -> stealth_metrics::DashboardStats;
}

impl MetricsExt for StealthMetricsCollector {
    fn record_privacy_event_error(&self, component: &str) {
        self.privacy_events_total
            .with_label_values(&[&format!("{}_error", component)])
            .inc();
    }

    fn record_friend_message_received(&self, friend_id: &str) {
        self.friend_messages_received_total
            .with_label_values(&[friend_id])
            .inc();
    }

    fn get_dashboard_stats(&self) -> stealth_metrics::DashboardStats {
        stealth_metrics::DashboardStats::from_collector(self)
    }
}