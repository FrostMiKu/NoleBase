//! Agent configuration and tool concurrency limits.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::agent::types::ToolExecutionPolicy;
use crate::provider::ApiFormat;

pub(crate) const DEFAULT_MAX_ROUNDS: u32 = 25;
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 200_000;
pub(crate) const DEFAULT_MAX_CONCURRENT_LOCAL_READS: usize = 8;
pub(crate) const DEFAULT_MAX_CONCURRENT_NETWORK_TOOLS: usize = 8;
pub(crate) const DEFAULT_MAX_CONCURRENT_SUBAGENTS: usize = 4;

#[derive(Clone)]
pub(crate) struct ToolConcurrencyLimits {
    pub(crate) local_reads: Arc<tokio::sync::Semaphore>,
    pub(crate) network: Arc<tokio::sync::Semaphore>,
    pub(crate) subagents: Arc<tokio::sync::Semaphore>,
}

impl ToolConcurrencyLimits {
    pub(crate) fn from_config(config: &AgentConfig) -> Self {
        Self::new(
            config.max_concurrent_local_reads,
            config.max_concurrent_network_tools,
            config.max_concurrent_subagents,
        )
    }

    pub(crate) fn new(local_reads: usize, network: usize, subagents: usize) -> Self {
        Self {
            local_reads: Arc::new(tokio::sync::Semaphore::new(local_reads)),
            network: Arc::new(tokio::sync::Semaphore::new(network)),
            subagents: Arc::new(tokio::sync::Semaphore::new(subagents)),
        }
    }

    pub(crate) async fn acquire(
        &self,
        policy: ToolExecutionPolicy,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let semaphore = match policy {
            ToolExecutionPolicy::Exclusive => return None,
            ToolExecutionPolicy::LocalRead => self.local_reads.clone(),
            ToolExecutionPolicy::Network => self.network.clone(),
            ToolExecutionPolicy::Subagent => self.subagents.clone(),
        };
        Some(
            semaphore
                .acquire_owned()
                .await
                .expect("tool concurrency semaphore closed"),
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    pub api_format: ApiFormat,
    pub api_key: String,
    #[serde(default)]
    pub tavily_api_key: String,
    pub model: String,
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_context_window_tokens")]
    pub context_window_tokens: u64,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default = "default_max_concurrent_local_reads")]
    pub max_concurrent_local_reads: usize,
    #[serde(default = "default_max_concurrent_network_tools")]
    pub max_concurrent_network_tools: usize,
    #[serde(default = "default_max_concurrent_subagents")]
    pub max_concurrent_subagents: usize,
    /// Whether the configured model accepts native image input. Defaults to
    /// false; vision capability is never guessed from the model name or
    /// protocol, so the user must enable it explicitly for a vision-capable
    /// model.
    #[serde(default)]
    pub supports_images: bool,
}

const fn default_max_tokens() -> u32 {
    8192
}

const fn default_context_window_tokens() -> u64 {
    DEFAULT_CONTEXT_WINDOW_TOKENS
}

const fn default_max_rounds() -> u32 {
    DEFAULT_MAX_ROUNDS
}

const fn default_max_concurrent_local_reads() -> usize {
    DEFAULT_MAX_CONCURRENT_LOCAL_READS
}

const fn default_max_concurrent_network_tools() -> usize {
    DEFAULT_MAX_CONCURRENT_NETWORK_TOOLS
}

const fn default_max_concurrent_subagents() -> usize {
    DEFAULT_MAX_CONCURRENT_SUBAGENTS
}

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading AI config {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parsing AI config {}", path.display()))?;
        if config.api_key.trim().is_empty() && config.api_format == ApiFormat::Messages {
            bail!("set api_key in {}", path.display());
        }
        if config.model.trim().is_empty() {
            bail!("model is empty in {}", path.display());
        }
        if config.base_url.trim().is_empty() {
            bail!("base_url is empty in {}", path.display());
        }
        if config.base_url.trim_end_matches('/').ends_with("/v1") {
            bail!("base_url must not include /v1");
        }
        if config.max_tokens == 0 {
            bail!("max_tokens must be greater than zero");
        }
        if config.context_window_tokens <= u64::from(config.max_tokens) {
            bail!("context_window_tokens must be greater than max_tokens");
        }
        if config.max_rounds == 0 {
            bail!("max_rounds must be greater than zero");
        }
        if config.max_concurrent_local_reads == 0 {
            bail!("max_concurrent_local_reads must be greater than zero");
        }
        if config.max_concurrent_network_tools == 0 {
            bail!("max_concurrent_network_tools must be greater than zero");
        }
        if config.max_concurrent_subagents == 0 {
            bail!("max_concurrent_subagents must be greater than zero");
        }
        Ok(config)
    }
}
