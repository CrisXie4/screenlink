//! Scrcpy 协议客户端：会话管理 + 线协议。
pub mod protocol;
pub mod session;

pub use protocol::{ControlMessage, ServerOptions};
pub use session::{start_session, DeviceEvent, SessionHandle, SessionStateInner, VideoInfo};
