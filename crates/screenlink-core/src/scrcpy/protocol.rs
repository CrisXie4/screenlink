//! Scrcpy v2.7 线协议实现（server 端字节布局对照 Genymobile/scrcpy v2.7 源码）。
//!
//! ## 对接点（Socket 通道设计）
//!
//! 1. **Server 启动**：`adb push scrcpy-server.jar /data/local/tmp/` 后
//!    `CLASSPATH=<jar> app_process / com.genymobile.scrcpy.Server 2.7 key=value...`
//!    启动设备端进程。`tunnel_forward=true` 时设备端创建 LocalServerSocket
//!    `scrcpy_<scid:08x>` 并等待连接，PC 端用 `adb forward tcp:L localabstract:scrcpy_<scid>`
//!    建立隧道，每个 TCP 连接对应设备端一个 socket。
//! 2. **连接顺序**：video -> (audio) -> control，每个 socket accept 后服务端先写 1 字节
//!    dummy（`send_dummy_byte=true`），PC 读到该字节即确认通道建立。
//! 3. **视频元数据**：video socket 上先读 64B 设备名（send_device_meta），再读
//!    12B 编解码头 = codecId(i32) + width(i32) + height(i32)。
//! 4. **帧封装**：每帧 = ptsAndFlags(u64, bit63=CONFIG / bit62=KEYFRAME) +
//!    len(u32) + Annex-B H.264 负载。CONFIG 帧即 SPS/PPS（WebCodecs 的 description）。
//! 5. **控制通道**：PC -> 设备为 ControlMessage（见 encode），设备 -> PC 为
//!    DeviceMessage（CLIPBOARD / ACK_CLIPBOARD / UHID_OUTPUT）。
//!    剪贴板双向同步即走此通道（clipboard_autosync=true 时设备剪贴板变化会主动上报）。

use crate::util::{parse_i64_be, parse_u32_be};

pub const SCRCPY_VERSION: &str = "2.7";

/// `putInt` 大端 4CC。H264 = "h264"
pub const CODEC_H264: u32 = 0x6832_3634;
pub const CODEC_H265: u32 = 0x6832_3635;
pub const CODEC_AV1: u32 = 0x6176_3031;

pub const PACKET_FLAG_CONFIG: u64 = 1 << 63;
pub const PACKET_FLAG_KEY_FRAME: u64 = 1 << 62;

// ---- Android 常量（KeyEvent / MotionEvent，与 scrcpy client 一致）----
pub mod and {
    pub const ACTION_DOWN: u8 = 0;
    pub const ACTION_UP: u8 = 1;
    pub const ACTION_MOVE: u8 = 2;

    pub const BUTTON_PRIMARY: i32 = 1;
    pub const BUTTON_SECONDARY: i32 = 2;
    pub const BUTTON_TERTIARY: i32 = 4;

    pub const KEYCODE_BACK: i32 = 4;
    pub const KEYCODE_HOME: i32 = 3;
    pub const KEYCODE_ENTER: i32 = 66;
    pub const KEYCODE_TAB: i32 = 61;
    pub const KEYCODE_ESC: i32 = 111;
    pub const KEYCODE_DEL: i32 = 67; // backspace
    pub const KEYCODE_FORWARD_DEL: i32 = 112;
    pub const KEYCODE_DPAD_UP: i32 = 19;
    pub const KEYCODE_DPAD_DOWN: i32 = 20;
    pub const KEYCODE_DPAD_LEFT: i32 = 21;
    pub const KEYCODE_DPAD_RIGHT: i32 = 22;
    pub const KEYCODE_PAGE_UP: i32 = 92;
    pub const KEYCODE_PAGE_DOWN: i32 = 93;
    pub const KEYCODE_MOVE_HOME: i32 = 122;
    pub const KEYCODE_MOVE_END: i32 = 123;
    pub const KEYCODE_VOLUME_UP: i32 = 24;
    pub const KEYCODE_VOLUME_DOWN: i32 = 25;
    pub const KEYCODE_POWER: i32 = 26;
    pub const KEYCODE_MENU: i32 = 82;
    pub const KEYCODE_APP_SWITCH: i32 = 187;
    pub const KEYCODE_SPACE: i32 = 62;

    pub const META_CTRL_ON: i32 = 0x1000;
    pub const AKEY_EVENT_FLAG_FROM_SYSTEM: i32 = 0x8;
}

// ---- ControlMessage 类型号（对照 ControlMessage.java）----
pub mod ctype {
    pub const INJECT_KEYCODE: u8 = 0;
    pub const INJECT_TEXT: u8 = 1;
    pub const INJECT_TOUCH_EVENT: u8 = 2;
    pub const INJECT_SCROLL_EVENT: u8 = 3;
    pub const BACK_OR_SCREEN_ON: u8 = 4;
    pub const EXPAND_NOTIFICATION_PANEL: u8 = 5;
    pub const EXPAND_SETTINGS_PANEL: u8 = 6;
    pub const COLLAPSE_PANELS: u8 = 7;
    pub const GET_CLIPBOARD: u8 = 8;
    pub const SET_CLIPBOARD: u8 = 9;
    pub const SET_SCREEN_POWER_MODE: u8 = 10;
    pub const ROTATE_DEVICE: u8 = 11;
}

/// 设备 -> PC 消息（对照 DeviceMessage.java / DeviceMessageWriter.java）
#[derive(Debug, Clone)]
pub enum DeviceMessage {
    /// u8 type=0 + u32 len + utf8
    Clipboard(String),
    /// u8 type=1 + u64 sequence
    AckClipboard(u64),
    Unknown(u8),
}

pub fn parse_device_message(buf: &[u8]) -> Option<(DeviceMessage, usize)> {
    if buf.is_empty() {
        return None;
    }
    match buf[0] {
        0 => {
            if buf.len() < 5 {
                return None;
            }
            let len = parse_u32_be(&buf[1..5]) as usize;
            if buf.len() < 5 + len {
                return None;
            }
            let text = String::from_utf8_lossy(&buf[5..5 + len]).into_owned();
            Some((DeviceMessage::Clipboard(text), 5 + len))
        }
        1 => {
            if buf.len() < 9 {
                return None;
            }
            Some((DeviceMessage::AckClipboard(parse_i64_be(&buf[1..9]) as u64), 9))
        }
        t => Some((DeviceMessage::Unknown(t), 1)),
    }
}

// ---- ControlMessage 编码 ----

#[derive(Debug, Clone)]
pub enum ControlMessage {
    /// type + action(u8) + keycode(i32) + repeat(i32) + metaState(i32)
    InjectKeycode { action: u8, keycode: i32, meta: i32 },
    /// type + len(u32) + utf8（上限 300B，超长由调用方拆分）
    InjectText(String),
    /// type + action(u8) + pointerId(u64) + pos(x i32,y i32,w u16,h u16)
    ///      + pressure(i16 固定点) + actionButton(i32) + buttons(i32)
    InjectTouch {
        action: u8,
        pointer_id: u64,
        x: i32,
        y: i32,
        screen_w: u16,
        screen_h: u16,
        pressure: u16,
        action_button: i32,
        buttons: i32,
    },
    /// type + pos + hScroll(i16) + vScroll(i16) + buttons(i32)
    InjectScroll {
        x: i32,
        y: i32,
        screen_w: u16,
        screen_h: u16,
        h_scroll: i16,
        v_scroll: i16,
        buttons: i32,
    },
    BackOrScreenOn(u8),
    ExpandNotificationPanel,
    CollapsePanels,
    /// copyKey: 0=NONE 1=COPY 2=CUT
    GetClipboard(u8),
    /// sequence=0 表示无需 ACK；paste=true 时设备直接粘贴
    SetClipboard { sequence: u64, text: String, paste: bool },
    SetScreenPowerMode(u8),
    RotateDevice,
}

impl ControlMessage {
    pub fn encode(self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64);
        match self {
            ControlMessage::InjectKeycode { action, keycode, meta } => {
                b.push(ctype::INJECT_KEYCODE);
                b.push(action);
                b.extend_from_slice(&keycode.to_be_bytes());
                b.extend_from_slice(&0i32.to_be_bytes()); // repeat
                b.extend_from_slice(&meta.to_be_bytes());
            }
            ControlMessage::InjectText(text) => {
                b.push(ctype::INJECT_TEXT);
                let raw = text.as_bytes();
                b.extend_from_slice(&(raw.len() as u32).to_be_bytes());
                b.extend_from_slice(raw);
            }
            ControlMessage::InjectTouch { action, pointer_id, x, y, screen_w, screen_h, pressure, action_button, buttons } => {
                b.push(ctype::INJECT_TOUCH_EVENT);
                b.push(action);
                b.extend_from_slice(&pointer_id.to_be_bytes());
                b.extend_from_slice(&x.to_be_bytes());
                b.extend_from_slice(&y.to_be_bytes());
                b.extend_from_slice(&screen_w.to_be_bytes());
                b.extend_from_slice(&screen_h.to_be_bytes());
                b.extend_from_slice(&pressure.to_be_bytes());
                b.extend_from_slice(&action_button.to_be_bytes());
                b.extend_from_slice(&buttons.to_be_bytes());
            }
            ControlMessage::InjectScroll { x, y, screen_w, screen_h, h_scroll, v_scroll, buttons } => {
                b.push(ctype::INJECT_SCROLL_EVENT);
                b.extend_from_slice(&x.to_be_bytes());
                b.extend_from_slice(&y.to_be_bytes());
                b.extend_from_slice(&screen_w.to_be_bytes());
                b.extend_from_slice(&screen_h.to_be_bytes());
                b.extend_from_slice(&h_scroll.to_be_bytes());
                b.extend_from_slice(&v_scroll.to_be_bytes());
                b.extend_from_slice(&buttons.to_be_bytes());
            }
            ControlMessage::BackOrScreenOn(action) => {
                b.push(ctype::BACK_OR_SCREEN_ON);
                b.push(action);
            }
            ControlMessage::ExpandNotificationPanel => b.push(ctype::EXPAND_NOTIFICATION_PANEL),
            ControlMessage::CollapsePanels => b.push(ctype::COLLAPSE_PANELS),
            ControlMessage::GetClipboard(copy_key) => {
                b.push(ctype::GET_CLIPBOARD);
                b.push(copy_key);
            }
            ControlMessage::SetClipboard { sequence, text, paste } => {
                b.push(ctype::SET_CLIPBOARD);
                b.extend_from_slice(&sequence.to_be_bytes());
                b.push(u8::from(paste));
                let raw = text.as_bytes();
                b.extend_from_slice(&(raw.len() as u32).to_be_bytes());
                b.extend_from_slice(raw);
            }
            ControlMessage::SetScreenPowerMode(mode) => {
                b.push(ctype::SET_SCREEN_POWER_MODE);
                b.push(mode);
            }
            ControlMessage::RotateDevice => b.push(ctype::ROTATE_DEVICE),
        }
        b
    }
}

/// u16 固定点（scrcpy Binary.floatToU16FixedPoint）：pressure 1.0 -> 0xFFFF
pub fn u16_fixed_point(v: f32) -> u16 {
    let f = (v * 65536.0).round() as i64;
    f.clamp(0, 0xFFFF) as u16
}

/// i16 固定点（scrcpy Binary.floatToI16FixedPoint）：1.0 -> 0x7FFF
pub fn i16_fixed_point(v: f32) -> i16 {
    let f = (v * 32768.0).round() as i64;
    f.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

// ---- Server 启动参数 ----

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub scid: u32,
    pub max_size: i32,
    pub bit_rate: i32,
    pub max_fps: f32,
    pub audio: bool,
    pub stay_awake: bool,
    pub show_touches: bool,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            scid: 0,
            max_size: 1600,
            bit_rate: 8_000_000,
            max_fps: 60.0,
            audio: false,
            stay_awake: true,
            show_touches: false,
        }
    }
}

impl ServerOptions {
    /// 生成 `app_process` 之后的参数表（首参必须是 SCRCPY_VERSION）。
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            SCRCPY_VERSION.to_string(),
            format!("scid={:08x}", self.scid),
            "log_level=info".into(),
            format!("max_size={}", self.max_size),
            format!("video_bit_rate={}", self.bit_rate),
            format!("max_fps={}", self.max_fps),
            "tunnel_forward=true".into(),
            "video=true".into(),
            format!("audio={}", self.audio),
            "control=true".into(),
            format!("stay_awake={}", self.stay_awake),
            format!("show_touches={}", self.show_touches),
            "clipboard_autosync=true".into(),
            "cleanup=true".into(),
            "power_on=true".into(),
            "send_device_meta=true".into(),
            "send_frame_meta=true".into(),
            "send_dummy_byte=true".into(),
            "send_codec_meta=true".into(),
            "downsize_on_error=true".into(),
            // 每秒一个关键帧：渲染端中途接入可快速恢复画面
            "video_codec_options=i-frame-interval=1".into(),
        ];
        args
    }
}
