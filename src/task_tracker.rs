use crate::{
    error::{Error, PrettyCause},
    shutdown::{ShutdownNotification, ShutdownReason},
};
use std::{borrow::Cow, future::Future, sync::Arc};
use tokio::{
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender},
        RwLock,
    },
    task::JoinHandle,
};

#[derive(Clone)]
pub struct TaskTracker {
    inner: tokio_util::task::TaskTracker,
    error_tx: UnboundedSender<(Cow<'static, str>, Error)>,
}

impl TaskTracker {
    pub fn new() -> (Self, UnboundedReceiver<(Cow<'static, str>, Error)>) {
        let (error_tx, error_rx) = tokio::sync::mpsc::unbounded_channel();
        let inner = tokio_util::task::TaskTracker::new();

        inner.close();

        (Self { inner, error_tx }, error_rx)
    }

    pub fn spawn(
        &self,
        name: impl Into<Cow<'static, str>> + Send + 'static,
        task: impl Future<Output = Result<Cow<'static, str>, Error>> + Send + 'static,
    ) -> JoinHandle<()> {
        let error_tx = self.error_tx.clone();

        self.inner.spawn(async move {
            match task.await {
                Ok(shutdown_message) => {
                    if !shutdown_message.is_empty() {
                        tracing::info!("{shutdown_message}");
                    }
                }
                Err(error) => error_tx.send((name.into(), error)).unwrap(),
            }
        })
    }

    pub async fn wait_with_notification(
        self,
        mut error_rx: UnboundedReceiver<(Cow<'static, str>, Error)>,
        shutdown_notification: ShutdownNotification, // Pass the full ShutdownNotification struct
    ) -> Arc<RwLock<ShutdownReason>> {
        drop(self.error_tx); // Drop the error_tx to avoid deadlocks

        let mut failed = false;

        while let Some((from, error)) = error_rx.recv().await {
            tracing::error!(
            "Received a fatal error from {from}:\n    {error:?}.{}",
            error.pretty_cause()
        );

            if failed || !shutdown_notification.is_ignited() { // Use is_ignited from ShutdownNotification
                tracing::info!("Initializing the shutdown...");

                failed = true;

                shutdown_notification.ignite().await;
            }
        }

        self.inner.wait().await;

        shutdown_notification.reason.clone() // Return the shutdown reason from ShutdownNotification
    }
}
