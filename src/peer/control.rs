use remagic_core::AppId;
use remagic_protocol::{Request, Response, MAX_FRAME};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone)]
pub struct ControlClient {
    socket: PathBuf,
}

impl ControlClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn close_complete(&self, app_id: &str) -> Result<(), ControlError> {
        let app_id = AppId::new(app_id.to_owned())?;
        let request = Request::Close {
            app_id,
            complete: true,
        };
        match self.request(&request)? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(ControlError::Rejected(message)),
            _ => Err(ControlError::UnexpectedResponse),
        }
    }

    fn request(&self, request: &Request) -> Result<Response, ControlError> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        write_frame(&mut stream, request)?;
        stream.shutdown(Shutdown::Write)?;
        read_frame(&mut stream)
    }
}

fn write_frame<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<(), ControlError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(ControlError::FrameLength(bytes.len()));
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<T: for<'de> serde::Deserialize<'de>>(
    stream: &mut UnixStream,
) -> Result<T, ControlError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(ControlError::FrameLength(length));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("control frame length is invalid: {0}")]
    FrameLength(usize),
    #[error("control request was rejected: {0}")]
    Rejected(String),
    #[error("control broker returned an unexpected response")]
    UnexpectedResponse,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    AppId(#[from] remagic_core::manifest::ManifestError),
}
