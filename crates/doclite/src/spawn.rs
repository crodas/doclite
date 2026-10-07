//! Where a rebuild runs, when the caller does not want to wait for it.
//!
//! Filling an index is a scan of every live record, and a caller that opens a
//! connection in a request handler does not want that on the critical path.
//! There is nothing here to spawn onto, though: ADR 0006 keeps the choice of
//! runtime with the caller, and no crate in this workspace depends on one. So
//! the caller brings the runtime, as one method.
//!
//! Not supplying one is the ordinary case and not a lesser one: the rebuild is
//! awaited inside `open` instead, which costs latency once and needs no gate at
//! all.

use std::future::Future;
use std::pin::Pin;

/// Somewhere a rebuild can run while the caller gets on with something else.
///
/// One method, taking a boxed future rather than a generic one, so a
/// [`Spawner`] can be held as a trait object beside the connection it fills.
///
/// Dropping the future instead of running it is allowed and is not silently
/// tolerated: the reads parked on an index that will now never be filled wake
/// as [`DocumentError::Rebuilding`](crate::DocumentError::Rebuilding) rather
/// than waiting forever.
///
/// ```
/// use std::future::Future;
/// use std::pin::Pin;
///
/// use doclite::Spawner;
///
/// struct OnTokio;
///
/// impl Spawner for OnTokio {
///     fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>) {
///         tokio::spawn(work);
///     }
/// }
/// ```
pub trait Spawner: Send + Sync + 'static {
    /// Runs this to completion somewhere that is not the caller.
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>);
}
