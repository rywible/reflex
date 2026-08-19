use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum FlyError {
    #[error("api error: {0}")]
    Api(String),
    #[error("machine not found: {0}")]
    MachineNotFound(String),
    #[error("budget quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("cleanup failed: remaining machines {0}")]
    CleanupFailed(usize),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GuestConfig {
    pub cpu_kind: String,
    pub cpus: u8,
    pub memory_mb: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineConfig {
    pub image: String,
    pub guest: GuestConfig,
    pub env: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub auto_destroy: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateMachineRequest {
    pub name: String,
    pub region: String,
    pub config: MachineConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineSummary {
    pub id: String,
    pub name: String,
    pub state: String, // "started", "stopped", "destroyed"
    pub region: String,
}

#[async_trait]
pub trait FlyApiClient: Send + Sync {
    async fn create_machine(&self, req: CreateMachineRequest) -> Result<MachineSummary, FlyError>;
    async fn stop_machine(&self, id: &str) -> Result<(), FlyError>;
    async fn destroy_machine(&self, id: &str) -> Result<(), FlyError>;
    async fn list_machines(&self) -> Result<Vec<MachineSummary>, FlyError>;
}

pub struct MockFlyClient {
    machines: RwLock<HashMap<String, MachineSummary>>,
    counter: AtomicUsize,
}

impl MockFlyClient {
    pub fn new() -> Self {
        Self {
            machines: RwLock::new(HashMap::new()),
            counter: AtomicUsize::new(1),
        }
    }
}

impl Default for MockFlyClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FlyApiClient for MockFlyClient {
    async fn create_machine(&self, req: CreateMachineRequest) -> Result<MachineSummary, FlyError> {
        let idx = self.counter.fetch_add(1, Ordering::SeqCst);
        let id = format!("fly-mach-{idx}");
        let summary = MachineSummary {
            id: id.clone(),
            name: req.name,
            state: "started".to_string(),
            region: req.region,
        };
        self.machines.write().await.insert(id, summary.clone());
        Ok(summary)
    }

    async fn stop_machine(&self, id: &str) -> Result<(), FlyError> {
        let mut guard = self.machines.write().await;
        if let Some(mach) = guard.get_mut(id) {
            mach.state = "stopped".to_string();
            Ok(())
        } else {
            Err(FlyError::MachineNotFound(id.to_string()))
        }
    }

    async fn destroy_machine(&self, id: &str) -> Result<(), FlyError> {
        let mut guard = self.machines.write().await;
        if guard.remove(id).is_some() {
            Ok(())
        } else {
            Err(FlyError::MachineNotFound(id.to_string()))
        }
    }

    async fn list_machines(&self) -> Result<Vec<MachineSummary>, FlyError> {
        let guard = self.machines.read().await;
        Ok(guard.values().cloned().collect())
    }
}

pub struct FlyHttpClient {
    api_token: String,
    app_name: String,
    base_url: String,
    http_client: reqwest::Client,
}

impl FlyHttpClient {
    pub fn new(api_token: String, app_name: String) -> Self {
        Self {
            api_token,
            app_name,
            base_url: "https://api.machines.dev/v1/apps".to_string(),
            http_client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl FlyApiClient for FlyHttpClient {
    async fn create_machine(&self, req: CreateMachineRequest) -> Result<MachineSummary, FlyError> {
        let url = format!("{}/{}/machines", self.base_url, self.app_name);
        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.api_token)
            .json(&req)
            .send()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(FlyError::Api(format!(
                "Fly API returned error status {}",
                resp.status()
            )));
        }

        let summary = resp
            .json::<MachineSummary>()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;
        Ok(summary)
    }

    async fn stop_machine(&self, id: &str) -> Result<(), FlyError> {
        let url = format!("{}/{}/machines/{}/stop", self.base_url, self.app_name, id);
        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(FlyError::Api(format!(
                "Fly API returned error status {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn destroy_machine(&self, id: &str) -> Result<(), FlyError> {
        let url = format!("{}/{}/machines/{}", self.base_url, self.app_name, id);
        let resp = self
            .http_client
            .delete(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(FlyError::Api(format!(
                "Fly API returned error status {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn list_machines(&self) -> Result<Vec<MachineSummary>, FlyError> {
        let url = format!("{}/{}/machines", self.base_url, self.app_name);
        let resp = self
            .http_client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(FlyError::Api(format!(
                "Fly API returned error status {}",
                resp.status()
            )));
        }

        let list = resp
            .json::<Vec<MachineSummary>>()
            .await
            .map_err(|e| FlyError::Api(e.to_string()))?;
        Ok(list)
    }
}

pub struct FleetController {
    client: Arc<dyn FlyApiClient>,
    max_machines: usize,
}

impl FleetController {
    pub fn new(client: Arc<dyn FlyApiClient>, max_machines: usize) -> Self {
        Self {
            client,
            max_machines,
        }
    }

    pub async fn launch_worker_pool(
        &self,
        count: usize,
        image_digest: &str,
    ) -> Result<Vec<MachineSummary>, FlyError> {
        if count > self.max_machines {
            return Err(FlyError::QuotaExceeded(format!(
                "requested {count} machines, max limit is {}",
                self.max_machines
            )));
        }

        let mut launched = Vec::new();
        for i in 0..count {
            let req = CreateMachineRequest {
                name: format!("reflex-worker-{i}"),
                region: "iad".to_string(),
                config: MachineConfig {
                    image: image_digest.to_string(),
                    guest: GuestConfig {
                        cpu_kind: "performance".to_string(),
                        cpus: 4,
                        memory_mb: 8192,
                    },
                    env: BTreeMap::new(),
                    metadata: BTreeMap::new(),
                    auto_destroy: true,
                },
            };
            let mach = self.client.create_machine(req).await?;
            launched.push(mach);
        }
        Ok(launched)
    }

    pub async fn active_worker_count(&self) -> Result<usize, FlyError> {
        let machines = self.client.list_machines().await?;
        Ok(machines.iter().filter(|m| m.state == "started").count())
    }

    pub async fn cleanup_all_workers(&self) -> Result<usize, FlyError> {
        let machines = self.client.list_machines().await?;
        let count = machines.len();
        for m in machines {
            let _ = self.client.stop_machine(&m.id).await;
            let _ = self.client.destroy_machine(&m.id).await;
        }

        let remaining = self.client.list_machines().await?;
        if !remaining.is_empty() {
            return Err(FlyError::CleanupFailed(remaining.len()));
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fleet_controller_launch_and_cleanup() {
        let client = Arc::new(MockFlyClient::new());
        let controller = FleetController::new(client, 20);

        let workers = controller
            .launch_worker_pool(20, "sha256:reflex-worker-image")
            .await
            .unwrap();
        assert_eq!(workers.len(), 20);

        let cleaned = controller.cleanup_all_workers().await.unwrap();
        assert_eq!(cleaned, 20);
    }
}
