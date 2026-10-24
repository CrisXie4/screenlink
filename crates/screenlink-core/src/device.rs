//! 设备模型：USB / WiFi / 模拟器 统一抽象 + 解析工具。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Usb,
    Wifi,
    Emulator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceState {
    Online,
    Offline,
    Unauthorized,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub serial: String,
    pub state: DeviceState,
    pub transport: Transport,
    pub model: String,
    pub manufacturer: String,
    pub android_version: String,
    pub sdk: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wifi_ip: Option<String>,
    pub mirroring: bool,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        let brand = if self.manufacturer.is_empty() {
            ""
        } else {
            &self.manufacturer
        };
        if brand.is_empty() {
            self.model.clone()
        } else {
            format!("{} {}", capitalize(brand), self.model)
        }
    }

    pub fn addr(&self) -> Option<String> {
        // WiFi 设备 serial 形如 192.168.1.5:5555
        if self.transport == Transport::Wifi {
            Some(self.serial.clone())
        } else {
            None
        }
    }
}

/// `adb devices -l` 行解析，例如：
/// `R3CN1234 device usb:1-2 product:foo model:Pixel_7 device:bar transport_id:3`
pub fn parse_devices_line(line: &str) -> Option<DeviceInfo> {
    let mut parts = line.split_whitespace();
    let serial = parts.next()?.to_string();
    let state = match parts.next()? {
        "device" => DeviceState::Online,
        "offline" => DeviceState::Offline,
        "unauthorized" => DeviceState::Unauthorized,
        _ => return None,
    };
    let mut model = String::new();
    let mut device_name = String::new();
    for tok in parts {
        if let Some(m) = tok.strip_prefix("model:") {
            model = m.replace('_', " ");
        } else if let Some(d) = tok.strip_prefix("device:") {
            device_name = d.to_string();
        }
    }
    if model.is_empty() {
        model = device_name;
    }
    let transport = if serial.starts_with("emulator-") {
        Transport::Emulator
    } else if serial.contains(':') && serial.split(':').nth(1).map(|p| p.parse::<u16>().is_ok()).unwrap_or(false) {
        Transport::Wifi
    } else {
        Transport::Usb
    };
    Some(DeviceInfo {
        serial,
        state,
        transport,
        model,
        manufacturer: String::new(),
        android_version: String::new(),
        sdk: String::new(),
        battery: None,
        wifi_ip: None,
        mirroring: false,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct MdnsService {
    pub name: String,
    pub service_type: String,
    pub addr: String,
    pub kind: MdnsKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MdnsKind {
    /// 可直接 connect 的已配对无线设备
    Connect,
    /// 需要配对码的待配对设备
    Pairing,
}

/// `adb mdns services` 行解析，例如：
/// `adb-1a2b3c4d-5e6f._adb-tls-connect._tcp.  192.168.1.5:37890`
pub fn parse_mdns_line(line: &str) -> Option<MdnsService> {
    let line = line.trim();
    if line.is_empty() || !line.contains(".tcp.") {
        return None;
    }
    let mut it = line.split_whitespace();
    let name = it.next()?.to_string();
    let addr = it.next()?.to_string();
    // 服务类型 = 实例名之后以 "_" 开头的部分，如 "_adb-tls-connect._tcp."
    let idx = name.find("._")?;
    let service_type = name[idx + 1..].to_string();
    let kind = if service_type.contains("pairing") {
        MdnsKind::Pairing
    } else {
        MdnsKind::Connect
    };
    Some(MdnsService {
        name,
        service_type,
        addr,
        kind,
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
