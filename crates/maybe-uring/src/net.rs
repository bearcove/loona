#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
mod net_uring;

#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
pub use net_uring::*;

#[cfg(not(all(target_os = "linux", feature = "tokio-uring")))]
mod net_noring;

#[cfg(not(all(target_os = "linux", feature = "tokio-uring")))]
pub use net_noring::*;
