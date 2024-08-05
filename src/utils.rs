use crate::error::{NotHex, UtilError};

pub fn unhex(hex_data: &str, what_is_hex: NotHex) -> Result<Vec<u8>, UtilError> {
    if let Some(stripped) = hex_data.strip_prefix("0x") {
        hex::decode(stripped).map_err(|_| UtilError::NotHex(what_is_hex))
    } else {
        hex::decode(hex_data).map_err(|_| UtilError::NotHex(what_is_hex))
    }
}

pub mod task_tracker {
    use super::shutdown::ShutdownNotification;
    use crate::error::{Error, PrettyCause, TaskError};
    use std::{
        error::Error as StdError,
        fmt::{Debug, Display},
        future::Future,
    };
    use tokio::{
        sync::mpsc::{
            self, error::TrySendError, Receiver, Sender, UnboundedReceiver, UnboundedSender,
        },
        task::JoinHandle,
    };
    use tokio_util::task::TaskTracker as InnerTaskTracker;

    /// Needed to place [`TaskName`]s in error types.
    pub trait Print: Display + Debug {}

    impl<T: Display + Debug> Print for T {}

    pub type TaskName = Box<dyn Print + Send>;

    fn create_closed() -> InnerTaskTracker {
        let tt = InnerTaskTracker::new();

        tt.close();

        tt
    }

    fn spawn_inner<T, E>(
        tt: &InnerTaskTracker,
        name: impl Print + Send + 'static,
        task: impl Future<Output = Result<T, E>> + Send + 'static,
        handler: impl FnOnce(T) + Send + 'static,
        send_error: impl Fn((TaskName, E)) + Send + 'static,
    ) -> JoinHandle<()> {
        tt.spawn(async move {
            match task.await {
                Ok(output) => handler(output),
                Err(error) => send_error((Box::new(name), error)),
            }
        })
    }

    async fn wait_inner<S>(
        tt: InnerTaskTracker,
        error_tx: S,
        handler: impl Future<Output = Result<(), Error>>,
    ) -> Result<(), Error> {
        // `self` holds the last `error_tx`, so we need to drop it; otherwise it'll create a
        // deadlock on `error_rx.recv()` (see `wait_inner()` callers).
        drop(error_tx);

        handler.await?;

        tt.wait().await;

        Ok(())
    }

    fn print_fatal_error<E: StdError>(from: &TaskName, error: &(impl PrettyCause<E> + Display)) {
        tracing::error!(
            "Received a fatal error from {from}:{}",
            error.pretty_cause()
        );
    }

    #[derive(Clone)]
    pub struct TaskTracker {
        inner: InnerTaskTracker,
        error_tx: UnboundedSender<(TaskName, Error)>,
    }

    impl TaskTracker {
        /// [`tokio::mpsc::UnboundedReceiver`] can't be cloned and hence stored inside the struct,
        /// so it's the caller's responsibility to take the receiver and then pass it to
        /// [`Self::wait_and_shutdown`].
        pub fn new() -> (Self, UnboundedReceiver<(TaskName, Error)>) {
            let (error_tx, error_rx) = mpsc::unbounded_channel();

            (
                Self {
                    inner: create_closed(),
                    error_tx,
                },
                error_rx,
            )
        }

        /// Will panic on an error occurrence inside `task` if a receiver handle from
        /// [`Self::new()`] has been closed/dropped before the occurrence.
        pub fn spawn(
            &self,
            name: impl Print + Send + 'static,
            task: impl Future<Output = Result<impl Display, Error>> + Send + 'static,
        ) -> JoinHandle<()> {
            let error_tx = self.error_tx.clone();

            spawn_inner(
                &self.inner,
                name,
                task,
                |shutdown_message| tracing::info!("{shutdown_message}"),
                move |error| {
                    error_tx
                        .send(error)
                        .expect("receiver handle shouldn't be closed/dropped");
                },
            )
        }

        pub async fn wait_and_shutdown(
            self,
            mut error_rx: UnboundedReceiver<(TaskName, Error)>,
            shutdown_notification: ShutdownNotification,
        ) {
            wait_inner(self.inner, self.error_tx, async {
                let mut failed = false;

                while let Some((from, error)) = error_rx.recv().await {
                    print_fatal_error(&from, &error);

                    if failed {
                        shutdown_notification
                            .ignite(|| {
                                tracing::info!("Initialising the shutdown...");

                                failed = true;
                            })
                            .await;
                    }
                }

                Ok(())
            })
            .await
            .expect("this future shouldn't return errors");
        }
    }

    type ErrorChannel = (
        Sender<(TaskName, TaskError)>,
        Receiver<(TaskName, TaskError)>,
    );

    #[allow(clippy::module_name_repetitions)]
    pub struct ShortTaskTracker {
        inner: InnerTaskTracker,
        error_channel: ErrorChannel,
    }

    impl ShortTaskTracker {
        pub fn new() -> Self {
            Self {
                inner: create_closed(),
                error_channel: mpsc::channel(1),
            }
        }

        pub fn spawn(
            &self,
            name: impl Print + Send + 'static,
            task: impl Future<Output = Result<(), TaskError>> + Send + 'static,
        ) -> JoinHandle<()> {
            let error_tx = self.error_channel.0.clone();

            spawn_inner(
                &self.inner,
                name,
                task,
                |()| (),
                move |tn_and_error| {
                    if let Err(
                        TrySendError::Full((from, error)) | TrySendError::Closed((from, error)),
                    ) = error_tx.try_send(tn_and_error)
                    {
                        print_fatal_error(&from, &error);
                    };
                },
            )
        }

        pub async fn try_wait(mut self) -> Result<(), Error> {
            wait_inner(self.inner, self.error_channel.0, async {
                if let Some((from, error)) = self.error_channel.1.recv().await {
                    return Err(Error::Task(from, error));
                }

                Ok(())
            })
            .await
        }
    }
}

pub mod logger {
    use super::shutdown;
    use crate::{callback, chain, database, server, Error};
    use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

    const TARGETS: &[&str] = &[
        callback::MODULE,
        database::MODULE,
        chain::MODULE,
        server::MODULE,
        shutdown::MODULE,
        env!("CARGO_PKG_NAME"),
    ];
    const COMMA: &str = ",";
    const INFO: &str = "=info";
    const OFF: &str = "off";

    pub fn initialize(directives: String) -> Result<(), Error> {
        let filter =
            EnvFilter::try_new(&directives).map_err(|e| Error::LoggerDirectives(directives, e))?;

        tracing_subscriber::fmt()
            .with_timer(UtcTime::rfc_3339())
            .with_env_filter(filter)
            .init();

        Ok(())
    }

    fn default_filter_capacity() -> usize {
        OFF.len().saturating_add(
            TARGETS
                .iter()
                .map(|module| {
                    COMMA
                        .len()
                        .saturating_add(module.len())
                        .saturating_add(INFO.len())
                })
                .sum(),
        )
    }

    pub fn default_filter() -> String {
        let mut filter = String::with_capacity(default_filter_capacity());

        filter.push_str(OFF);

        for target in TARGETS {
            filter.push_str(COMMA);
            filter.push_str(target);
            filter.push_str(INFO);
        }

        filter
    }

    #[cfg(test)]
    mod tests {
        use tracing_subscriber::EnvFilter;

        #[test]
        fn default_filter_capacity() {
            assert_eq!(
                super::default_filter().len(),
                super::default_filter_capacity()
            );
        }

        #[test]
        fn default_filter_is_valid() {
            assert!(EnvFilter::try_new(super::default_filter()).is_ok());
        }
    }
}

pub mod shutdown {
    use crate::Error;
    use std::{
        fmt::{Display, Formatter, Result as FmtResult},
        io::Result as IoResult,
        panic::{self, PanicInfo},
        sync::Arc,
        time::Duration,
    };
    use tokio::{signal, sync::RwLock, task, time};
    use tokio_util::sync::CancellationToken;

    pub const MODULE: &str = module_path!();

    const TIP_FUSE_SECS: u64 = 15;

    #[derive(Clone)]
    #[allow(clippy::module_name_repetitions)]
    pub struct ShutdownNotification {
        pub token: CancellationToken,
        pub outcome: Arc<RwLock<ShutdownOutcome>>,
    }

    impl ShutdownNotification {
        pub fn new() -> Self {
            Self {
                token: CancellationToken::new(),
                outcome: Arc::new(RwLock::new(ShutdownOutcome::UserRequested)),
            }
        }

        pub async fn ignite(&self, f: impl FnOnce()) {
            let mut outcome = self.outcome.write().await;

            if matches!(*outcome, ShutdownOutcome::UserRequested) {
                f();

                *outcome = ShutdownOutcome::UnrecoverableError { panic: false };

                self.token.cancel();
            }
        }
    }

    pub async fn listener(
        shutdown_notification: CancellationToken,
        shutdown_completed: CancellationToken,
    ) -> Result<(), Error> {
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
                () = time::sleep(Duration::from_secs(TIP_FUSE_SECS)) => {
                    tracing::warn!(
                        "TIP: Send the shutdown signal one more time to early terminate the daemon instead of waiting for the graceful shutdown."
                    );
                }
            }
        });

        tokio::select! {
            biased;
            () = shutdown_completed.cancelled() => {}
            result = signal::ctrl_c() => {
                process_signal(result)?;

                tracing::info!("Received the second shutdown signal. Terminating the daemon...");

                return Ok(());
            }
        }

        tip.await.expect("tip task shouldn't panic");

        tracing::debug!("The shutdown signal listener is shut down.");

        Ok(())
    }

    fn process_signal(result: IoResult<()>) -> Result<(), Error> {
        result.map_err(Error::ShutdownSignal)?;

        // Print shutdown log messages on the next line after the Control-C command.
        println!();

        Ok(())
    }

    #[derive(Clone, Copy)]
    #[allow(clippy::module_name_repetitions)]
    pub enum ShutdownOutcome {
        UserRequested,
        UnrecoverableError { panic: bool },
    }

    pub fn set_panic_hook(
        print: impl Fn(PrettyPanic<'_>) + Send + Sync + 'static,
        shutdown_notification: ShutdownNotification,
    ) {
        panic::set_hook(Box::new(move |panic_info| {
            // `task::block_in_place()` is needed here since panic may be called within an async
            // context that'll result in a panic in `blocking_*()` methods (see their docs).

            let is_panicking =
                |outcome| matches!(outcome, ShutdownOutcome::UnrecoverableError { panic: true });

            let first = if is_panicking(task::block_in_place(|| {
                *shutdown_notification.outcome.blocking_read()
            })) {
                false
            } else {
                let mut outcome =
                    task::block_in_place(|| shutdown_notification.outcome.blocking_write());

                if is_panicking(*outcome) {
                    false
                } else {
                    *outcome = ShutdownOutcome::UnrecoverableError { panic: true };

                    shutdown_notification.token.cancel();

                    true
                }
            };

            print(PrettyPanic { panic_info, first });
        }));
    }

    pub struct PrettyPanic<'a> {
        panic_info: &'a PanicInfo<'a>,
        first: bool,
    }

    // It looks like it's impossible to acquire `PanicInfo` outside of `panic::set_hook`, which
    // could alter execution of other unit tests, so, without mocking the `panic_info` field,
    // there's no way to test the `Display`ing.
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
}
