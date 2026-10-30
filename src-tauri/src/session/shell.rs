use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::LinkError;

/// Bytes from a remote shell. Never log these. The user may type a password.
pub(crate) enum ShellBytes {
    Data(Vec<u8>),
    Closed,
}

pub(crate) trait ShellWriter: Send + Sync {
    fn write<'a>(
        &'a self,
        data: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;

    fn resize<'a>(
        &'a self,
        cols: u32,
        rows: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>>;

    fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// A login shell on an existing SSH connection. There is no command string.
pub(crate) struct OpenedShell {
    pub writer: Arc<dyn ShellWriter>,
    pub incoming: mpsc::Receiver<ShellBytes>,
}
