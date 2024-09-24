use crate::error::Error;
use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    io::Result as IoResult,
    panic::{self, PanicInfo},
    process,
    sync::Arc,
    time::Duration,
};
use tokio::{signal, sync::RwLock, time};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ShutdownNotification {
    pub token: CancellationToken,
    pub reason: Arc<RwLock<ShutdownReason>>,
}

impl ShutdownNotification {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: Arc::new(RwLock::new(ShutdownReason::UserRequested)),
        }
    }

    pub fn is_ignited(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn ignite(&self) {
        *self.reason.write().await = ShutdownReason::UnrecoverableError;
        self.token.cancel();
    }
}

pub async fn listener(
    shutdown_notification: CancellationToken,
    shutdown_completed: CancellationToken,
) -> Result<(), Error> {
    const TIP_TIMEOUT_SECS: u64 = 30;

    tokio::select! {
        biased;
        result = signal::ctrl_c() => {
            process_signal(result)?;

            tracing::info!("Received the shutdown signal. Initialising the shutdown...");

            shutdown_notification.cancel();
        }
        () = shutdown_notification.cancelled() => {}
    }

    let shutdown_completed_clone = shutdown_completed.clone();
    let tip = tokio::spawn(async move {
        tokio::select! {
            biased;
            () = shutdown_completed_clone.cancelled() => {}
            () = time::sleep(Duration::from_secs(TIP_TIMEOUT_SECS)) => {
                tracing::warn!(
                    "Send the shutdown signal one more time to kill the daemon instead of waiting for the graceful shutdown."
                );
            }
        }
    });

    tokio::select! {
        biased;
        () = shutdown_completed.cancelled() => {}
        result = signal::ctrl_c() => {
            process_signal(result)?;

            tracing::info!("Received the second shutdown signal. Killing the daemon...");

            process::abort()
        }
    }

    tip.await.expect("tip task shouldn't panic");

    tracing::info!("The shutdown signal listener is shut down.");

    Ok(())
}

fn process_signal(result: IoResult<()>) -> Result<(), Error> {
    result.map_err(Error::ShutdownSignal)?;

    println!(); // Print shutdown log messages on the next line after the Control-C command.

    Ok(())
}

#[derive(Clone, Copy)]
#[allow(clippy::module_name_repetitions)]
pub enum ShutdownReason {
    UserRequested,
    UnrecoverableError,
}

pub fn set_panic_hook(
    print: impl Fn(PrettyPanic<'_>) + Send + Sync + 'static,
    shutdown_notification: ShutdownNotification,
) {
    panic::set_hook(Box::new(move |panic_info| {
        let reason = *shutdown_notification.reason.blocking_read();

        let first = match reason {
            ShutdownReason::UserRequested => false,
            ShutdownReason::UnrecoverableError => {
                *shutdown_notification.reason.blocking_write() = ShutdownReason::UnrecoverableError;
                true
            }
        };

        print(PrettyPanic { panic_info, first });

        shutdown_notification.token.cancel();
    }));
}

pub struct PrettyPanic<'a> {
    panic_info: &'a PanicInfo<'a>,
    first: bool,
}

impl Display for PrettyPanic<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str("A panic detected")?;

        if let Some(location) = self.panic_info.location() {
            f.write_str(" at ")?;
            location.fmt(f)?;
        }

        let payload = self.panic_info.payload();
        let message_option = match payload.downcast_ref() {
            Some(string) => Some(*string),
            None => payload.downcast_ref::<String>().map(|string| &string[..]),
        };

        if let Some(panic_message) = message_option {
            f.write_str(":\n    ")?;
            f.write_str(panic_message)?;
        }

        f.write_str(".")?;

        // Print the report request only on the first panic.
        if self.first {
            f.write_str(concat!(
            "\n\nThis is a bug. Please report it at ",
            env!("CARGO_PKG_REPOSITORY"),
            "/issues."
            ))?;
        }

        Ok(())
    }
}
