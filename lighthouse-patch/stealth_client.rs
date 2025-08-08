use reqwest::Client;
use serde_json::json;
use types::{Attestation, EthSpec};
use anyhow::Result;

/// Client for communicating with the stealth sidecar
#[derive(Clone)]
pub struct StealthClient {
    client: Client,
    base_url: String,
}

impl StealthClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.to_string(),
        })
    }

    /// Publish attestation via stealth sidecar with privacy protections
    pub async fn publish_with_privacy<E: EthSpec>(&self, attestation: &Attestation<E>) -> Result<()> {
        let url = format!("{}/api/v1/stealth_publish", self.base_url);
        
        let response = self.client
            .post(&url)
            .json(&json!({
                "attestation": attestation,
                "stealth_mode": "full", // Enable both subnet shuffling + friend mesh
                "priority": "timing_defense" // Prioritize defeating timing attacks
            }))
            .timeout(std::time::Duration::from_millis(200)) // Fast timeout to avoid slowing attestations
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Stealth publish failed: {}", response.status()))
        }
    }
}