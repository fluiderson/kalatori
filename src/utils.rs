//! The module for miscellaneous utilities.

use crate::error::{NotHex, UtilError};
use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    path::{self, Path},
};

pub fn unhex(hex_data: &str, what_is_hex: NotHex) -> Result<Vec<u8>, UtilError> {
    if let Some(stripped) = hex_data.strip_prefix("0x") {
        const_hex::decode(stripped).map_err(|_| UtilError::NotHex(what_is_hex))
    } else {
        const_hex::decode(hex_data).map_err(|_| UtilError::NotHex(what_is_hex))
    }
}

pub struct PathDisplay<T>(pub T);

impl<T: AsRef<Path>> Display for PathDisplay<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let path = self.0.as_ref();

        match path::absolute(path) {
            Ok(absolute) => absolute.display().fmt(f),
            Err(_) => path.display().fmt(f),
        }
    }
}

pub mod task_tracker {
    //! The task tracker module.
    //!
    //! Contains utilities for orchestrating crucial initialization & loop tasks, and logic for
    //! handling errors occurring in them.

    use super::shutdown::{ShutdownNotification, ShutdownOutcome};
    use crate::error::{Error, PrettyCause, TaskError};
    use async_lock::RwLockUpgradableReadGuard;
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

    /// The main task tracker for processing the core main loop.
    ///
    /// Only 1 should be used across the program (see [`Self::wait_and_shutdown`]). Otherwise, the
    /// shutdown behaviour is unknown.
    #[derive(Clone)]
    pub struct TaskTracker {
        inner: InnerTaskTracker,
        error_tx: UnboundedSender<(TaskName, Error)>,
    }

    impl TaskTracker {
        /// [`tokio::mpsc::UnboundedReceiver`] can't be cloned and hence isn't stored inside the
        /// struct, so it's the caller's responsibility to take the receiver and then pass it to
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

        /// Starts the program main loop & waits until all tasks are finished.
        ///
        /// If any errors would occur in awaited tasks, `shutdown_notification` is triggered to shut
        /// down all listening it tasks, and, eventually, the program.
        pub async fn wait_and_shutdown(
            self,
            mut error_rx: UnboundedReceiver<(TaskName, Error)>,
            shutdown_notification: ShutdownNotification,
        ) {
            wait_inner(self.inner, self.error_tx, async {
                let mut no_errors = true;

                while let Some((from, error)) = error_rx.recv().await {
                    print_fatal_error(&from, &error);

                    if no_errors {
                        let outcome = shutdown_notification.outcome.upgradable_read().await;

                        if matches!(*outcome, ShutdownOutcome::UserRequested) {
                            *RwLockUpgradableReadGuard::upgrade(outcome).await =
                                ShutdownOutcome::UnrecoverableError { panic: false };

                            shutdown_notification.token.cancel();
                        }

                        no_errors = false;
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

    /// The short-lived task tracker.
    ///
    /// Should be used for tracking temporal tasks (e.g. initialization ones).
    #[expect(clippy::module_name_repetitions)]
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

        /// Waits until all [`spawn`](Self::spawn)ed tasks are finished.
        ///
        /// If an error occurs in awaited tasks, this method immediately returns the first error. Do
        /// note that it doesn't cancel unfinished tasks, they will continue running until
        /// completion & log all occured errors.
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
    use crate::{callback, chain_wip, database, server, Error};
    use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

    const TARGETS: &[&str] = &[
        callback::MODULE,
        database::MODULE,
        chain_wip::MODULE,
        server::MODULE,
        shutdown::MODULE,
        env!("CARGO_PKG_NAME"),
    ];
    const COMMA: &str = ",";
    const INFO: &str = "=info";
    const OFF: &str = "off";

    pub fn initialize(directives: impl AsRef<str>) -> Result<(), Error> {
        let filter = EnvFilter::try_new(directives)?;

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
    //! The shutdown module.
    //!
    //! Handles a graceful shutdown with an asynchronous runtime with respect of panics (by
    //! incorporating a custom panic handler) & unrecoverable errors from another modules.

    use crate::Error;
    use async_lock::{RwLock, RwLockUpgradableReadGuard};
    use std::{
        fmt::{Display, Formatter, Result as FmtResult},
        io::Result as IoResult,
        panic::{self, PanicHookInfo},
        sync::Arc,
        time::Duration,
    };
    use tokio::{signal, time};
    use tokio_util::sync::CancellationToken;

    pub const MODULE: &str = module_path!();

    const TIP_FUSE_SECS: u64 = 15;

    #[derive(Clone)]
    #[expect(clippy::module_name_repetitions)]
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
            () = shutdown_notification.cancelled() => {
                tracing::info!("The shutdown started by an unrecoverable error...");
            }
        }

        let shutdown_completed_clone = shutdown_completed.clone();
        let tip = tokio::spawn(async move {
            tokio::select! {
                biased;
                () = shutdown_completed_clone.cancelled() => {}
                () = time::sleep(Duration::from_secs(TIP_FUSE_SECS)) => {
                    tracing::warn!(
                        "Send the shutdown signal one more time to early terminate the daemon \
                        instead of waiting for the graceful shutdown."
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
    #[expect(clippy::module_name_repetitions)]
    pub enum ShutdownOutcome {
        UserRequested,
        UnrecoverableError { panic: bool },
    }

    pub fn set_panic_hook(
        print: impl Fn(PrettyPanic<'_>) + Send + Sync + 'static,
        shutdown_notification: ShutdownNotification,
    ) {
        panic::set_hook(Box::new(move |panic_info| {
            let outcome = shutdown_notification.outcome.upgradable_read_blocking();

            let first = if matches!(
                *outcome,
                ShutdownOutcome::UnrecoverableError { panic: true }
            ) {
                false
            } else {
                *RwLockUpgradableReadGuard::upgrade_blocking(outcome) =
                    ShutdownOutcome::UnrecoverableError { panic: true };

                shutdown_notification.token.cancel();

                true
            };

            print(PrettyPanic { panic_info, first });
        }));
    }

    pub struct PrettyPanic<'a> {
        panic_info: &'a PanicHookInfo<'a>,
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
