use super::control::ControlError;
use super::noise::NoiseError;
use super::protocol::ProtocolError;
use super::reading::ReadingError;
use super::storage::StorageError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("已有同步正在进行")]
    Busy,
    #[error("请在两台设备确认配对码 {0}")]
    PairingRequired(String),
    #[error("对端设备身份与发现记录不一致")]
    IdentityMismatch,
    #[error("对端没有可连接的局域网地址")]
    NoAddress,
    #[error("同步对象不存在")]
    MissingObject,
    #[error("未选择任何同步项")]
    EmptySelection,
    #[error("对端拒绝同步：{0}")]
    Remote(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error(transparent)]
    Noise(#[from] NoiseError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Reading(#[from] ReadingError),
    #[error(transparent)]
    Control(#[from] ControlError),
}

impl PeerError {
    pub fn user_message(&self) -> String {
        let mut current: &(dyn std::error::Error + 'static) = self;
        loop {
            if let Some(error) = current.downcast_ref::<std::io::Error>() {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    return "网络传输超时；请重试，已接收部分会从断点继续".into();
                }
                return format!("网络连接失败：{error}");
            }
            let Some(source) = current.source() else {
                return self.to_string();
            };
            current = source;
        }
    }
}
