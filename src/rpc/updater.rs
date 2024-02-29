use super::{fetch_best_runtime, ApiProperties};
use crate::{unexpected_closure_of_notification_channel, OnlineClient, RuntimeConfig};
use anyhow::{Context, Result};
use std::sync::Arc;
use subxt::{
    backend::{
        legacy::{LegacyBackend, LegacyRpcMethods},
        Backend,
    },
    constants::ConstantsClient,
};
use tokio::sync::{watch::Receiver, RwLock};

pub struct Updater {
    pub methods: LegacyRpcMethods<RuntimeConfig>,
    pub backend: Arc<LegacyBackend<RuntimeConfig>>,
    pub client: OnlineClient,
    pub properties: Arc<RwLock<ApiProperties>>,
    pub constants: ConstantsClient<RuntimeConfig, OnlineClient>,
    pub no_utility: bool,
    pub shutdown_notification: Receiver<bool>,
}

impl Updater {
    pub async fn ignite(mut self) -> Result<&'static str> {
        loop {
            let mut updates = self
                .backend
                .stream_runtime_version()
                .await
                .context("failed to get the runtime updates stream")?;

            if let Some(current_runtime_version_result) = updates.next().await {
                let current_runtime_version = current_runtime_version_result
                    .context("failed to decode the current runtime version")?;

                // The updates stream is always returns the current runtime version in the first
                // item. We don't skip it though because during a connection loss or prolonged
                // daemon startup the runtime can be updated, hence this condition will catch this.
                if self.client.runtime_version() != current_runtime_version {
                    self.process_update().await?;
                }

                loop {
                    tokio::select! {
                        biased;
                        notification = self.shutdown_notification.changed() => {
                            return notification
                                .with_context(||
                                    unexpected_closure_of_notification_channel("update")
                                )
                                .map(|()| "The API client updater is shut down.");
                        }
                        runtime_version = updates.next() => {
                            if runtime_version.is_some() {
                                self.process_update().await?;
                            } else {
                                break;
                            }
                        }
                    }
                }
            }

            log::warn!(
                "Lost the connection while listening the endpoint for API client runtime updates. Retrying..."
            );
        }
    }

    async fn process_update(&self) -> Result<()> {
        // We don't use the runtime version from the updates stream because it doesn't provide the
        // best block hash, so we fetch it ourselves (in `fetch_runtime`) and use it to make sure
        // that metadata & the runtime version are from the same block.
        let (metadata, runtime_version) = fetch_best_runtime(&self.methods, &*self.backend)
            .await
            .context("failed to fetch a new runtime for the API client")?;

        let mut current_properties = self.properties.write().await;

        self.client.set_metadata(metadata);
        self.client.set_runtime_version(runtime_version);

        // Do NOT inline `current_properties` here. It's used as a RAII wrapper for `self.client`.
        *current_properties = ApiProperties::fetch(&self.constants, self.no_utility)?;

        log::debug!("Properties from an API client update: {current_properties:?}.");

        Ok(())
    }
}
