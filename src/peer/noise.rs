use snow::{params::NoiseParams, Builder, HandshakeState, TransportState};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use thiserror::Error;

pub const MAGIC: &[u8; 4] = b"RMS1";
const MAX_CIPHERTEXT: usize = 65_535;

pub struct NoiseChannel {
    stream: TcpStream,
    state: TransportState,
    remote_static: Vec<u8>,
}

impl NoiseChannel {
    pub fn initiate(mut stream: TcpStream, private_key: &[u8]) -> Result<Self, NoiseError> {
        stream.write_all(MAGIC)?;
        let mut handshake = builder(private_key)?.build_initiator()?;
        write_handshake(&mut stream, &mut handshake)?;
        read_handshake(&mut stream, &mut handshake)?;
        write_handshake(&mut stream, &mut handshake)?;
        finish(stream, handshake)
    }

    pub fn accept(mut stream: TcpStream, private_key: &[u8]) -> Result<Self, NoiseError> {
        let mut magic = [0_u8; 4];
        stream.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(NoiseError::InvalidMagic);
        }
        let mut handshake = builder(private_key)?.build_responder()?;
        read_handshake(&mut stream, &mut handshake)?;
        write_handshake(&mut stream, &mut handshake)?;
        read_handshake(&mut stream, &mut handshake)?;
        finish(stream, handshake)
    }

    pub fn remote_static(&self) -> &[u8] {
        &self.remote_static
    }

    pub fn send<T: serde::Serialize>(&mut self, value: &T) -> Result<(), NoiseError> {
        let plain = bincode::serialize(value)?;
        if plain.is_empty() || plain.len() > 60_000 {
            return Err(NoiseError::FrameLength(plain.len()));
        }
        let mut encrypted = vec![0_u8; plain.len() + 64];
        let size = self.state.write_message(&plain, &mut encrypted)?;
        write_raw(&mut self.stream, &encrypted[..size])
    }

    pub fn receive<T: for<'de> serde::Deserialize<'de>>(&mut self) -> Result<T, NoiseError> {
        let encrypted = read_raw(&mut self.stream)?;
        let mut plain = vec![0_u8; encrypted.len()];
        let size = self.state.read_message(&encrypted, &mut plain)?;
        Ok(bincode::deserialize(&plain[..size])?)
    }
}

fn builder(private_key: &[u8]) -> Result<Builder<'_>, NoiseError> {
    if private_key.len() != 32 {
        return Err(NoiseError::InvalidPrivateKey);
    }
    let parameters: NoiseParams = "Noise_XX_25519_ChaChaPoly_BLAKE2s".parse()?;
    Ok(Builder::new(parameters).local_private_key(private_key))
}

fn write_handshake(stream: &mut TcpStream, state: &mut HandshakeState) -> Result<(), NoiseError> {
    let mut output = vec![0_u8; MAX_CIPHERTEXT];
    let size = state.write_message(&[], &mut output)?;
    write_raw(stream, &output[..size])
}

fn read_handshake(stream: &mut TcpStream, state: &mut HandshakeState) -> Result<(), NoiseError> {
    let message = read_raw(stream)?;
    let mut output = vec![0_u8; MAX_CIPHERTEXT];
    state.read_message(&message, &mut output)?;
    Ok(())
}

fn finish(stream: TcpStream, state: HandshakeState) -> Result<NoiseChannel, NoiseError> {
    let remote_static = state
        .get_remote_static()
        .ok_or(NoiseError::MissingRemoteStatic)?
        .to_vec();
    Ok(NoiseChannel {
        stream,
        state: state.into_transport_mode()?,
        remote_static,
    })
}

fn write_raw(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), NoiseError> {
    if bytes.is_empty() || bytes.len() > MAX_CIPHERTEXT {
        return Err(NoiseError::FrameLength(bytes.len()));
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_raw(stream: &mut TcpStream) -> Result<Vec<u8>, NoiseError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_CIPHERTEXT {
        return Err(NoiseError::FrameLength(length));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[derive(Debug, Error)]
pub enum NoiseError {
    #[error("invalid ReMagic sync protocol marker")]
    InvalidMagic,
    #[error("Noise private key must contain 32 bytes")]
    InvalidPrivateKey,
    #[error("Noise XX handshake did not authenticate the remote static key")]
    MissingRemoteStatic,
    #[error("encrypted frame length is invalid: {0}")]
    FrameLength(usize),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Noise(#[from] snow::Error),
    #[error(transparent)]
    Bincode(#[from] Box<bincode::ErrorKind>),
}
