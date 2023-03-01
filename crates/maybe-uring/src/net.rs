use crate::io::IntoHalves;

#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
mod net_uring;

#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
pub use net_uring::*;

#[cfg(not(all(target_os = "linux", feature = "tokio-uring")))]
mod net_noring;

#[cfg(not(all(target_os = "linux", feature = "tokio-uring")))]
pub use net_noring::*;

impl IntoHalves for tokio::net::TcpStream {
    type Read = tokio::net::tcp::OwnedReadHalf;
    type Write = tokio::net::tcp::OwnedWriteHalf;

    fn into_halves(self) -> (Self::Read, Self::Write) {
        self.into_split()
    }
}
