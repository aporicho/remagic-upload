use super::PeerError;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub(super) fn connect_any(addresses: &[impl ToSocketAddrs]) -> Result<TcpStream, PeerError> {
    for address in addresses {
        if let Ok(stream) = TcpStream::connect_timeout(
            &address
                .to_socket_addrs()?
                .next()
                .ok_or(PeerError::NoAddress)?,
            Duration::from_secs(5),
        ) {
            return Ok(stream);
        }
    }
    Err(PeerError::NoAddress)
}

pub(super) fn configure(stream: &TcpStream) -> Result<(), std::io::Error> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    Ok(())
}
