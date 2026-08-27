use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{Value, json};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlEndpoint {
    pub host: String,
    pub port: u16,
}

impl ControlEndpoint {
    pub fn display_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Refresh,
    Status,
    Navigate,
    Stop,
    Recording,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Refresh => "刷新状态",
            Self::Status => "同步状态",
            Self::Navigate => "提交导航",
            Self::Stop => "停止导航",
            Self::Recording => "更新录制",
        }
    }
}

#[derive(Clone, Debug)]
pub enum ControlCommand {
    Refresh,
    Status,
    Navigate { goal_text: String, dry_run: bool },
    Stop,
    SetRecording(bool),
}

impl ControlCommand {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::Refresh => ActionKind::Refresh,
            Self::Status => ActionKind::Status,
            Self::Navigate { .. } => ActionKind::Navigate,
            Self::Stop => ActionKind::Stop,
            Self::SetRecording(_) => ActionKind::Recording,
        }
    }
}

#[derive(Debug)]
pub struct ControlResult {
    pub generation: u64,
    pub kind: ActionKind,
    pub result: Result<ResponseData, String>,
}

#[derive(Debug, Default)]
pub struct ResponseData {
    pub summary: String,
    pub summary_is_error: bool,
    pub labels: Option<Vec<String>>,
    pub recording_enabled: Option<bool>,
    pub navigation_running: Option<bool>,
}

pub struct ControlClient {
    endpoint: ControlEndpoint,
    generation: u64,
    sender: Sender<ControlResult>,
    receiver: Receiver<ControlResult>,
}

impl ControlClient {
    pub fn new(endpoint: ControlEndpoint) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            endpoint,
            generation: 0,
            sender,
            receiver,
        }
    }

    pub fn endpoint(&self) -> &ControlEndpoint {
        &self.endpoint
    }

    pub fn set_endpoint(&mut self, endpoint: ControlEndpoint) {
        self.endpoint = endpoint;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn dispatch(&self, command: ControlCommand, repaint: rerun::external::egui::Context) {
        let endpoint = self.endpoint.clone();
        let generation = self.generation;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let kind = command.kind();
            let result = execute(&endpoint, command);
            let _ = sender.send(ControlResult {
                generation,
                kind,
                result,
            });
            repaint.request_repaint();
        });
    }

    pub fn try_recv(&self) -> Option<ControlResult> {
        self.receiver.try_recv().ok()
    }
}

fn execute(endpoint: &ControlEndpoint, command: ControlCommand) -> Result<ResponseData, String> {
    match command {
        ControlCommand::Refresh => refresh(endpoint),
        ControlCommand::Status => operator_status(endpoint),
        ControlCommand::Navigate { goal_text, dry_run } => {
            let body = json!({"goal_text": goal_text, "dry_run": dry_run}).to_string();
            let response = request(endpoint, "POST", "/viz/api/navigation/text", Some(&body))?;
            let value: Value = serde_json::from_str(&response)
                .map_err(|err| format!("导航响应不是有效 JSON：{err}"))?;
            let accepted = value
                .get("accepted")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(ResponseData {
                summary: summarize_json(&response),
                summary_is_error: !accepted,
                navigation_running: accepted.then_some(true),
                ..Default::default()
            })
        }
        ControlCommand::Stop => {
            let response = request(endpoint, "POST", "/viz/api/navigation/stop", Some("{}"))?;
            Ok(ResponseData {
                summary: summarize_json(&response),
                navigation_running: Some(false),
                ..Default::default()
            })
        }
        ControlCommand::SetRecording(enabled) => {
            let body = json!({"enabled": enabled}).to_string();
            let response = request(
                endpoint,
                "POST",
                "/viz/api/dynamic-map/recording",
                Some(&body),
            )?;
            let value: Value = serde_json::from_str(&response)
                .map_err(|err| format!("录制响应不是有效 JSON：{err}"))?;
            Ok(ResponseData {
                summary: if enabled {
                    "动态地图录制已开启".to_owned()
                } else {
                    "动态地图录制已关闭".to_owned()
                },
                recording_enabled: value.get("enabled").and_then(Value::as_bool),
                ..Default::default()
            })
        }
    }
}

fn refresh(endpoint: &ControlEndpoint) -> Result<ResponseData, String> {
    let metadata = request(endpoint, "GET", "/viz/api/map/metadata", None)?;
    let metadata: Value =
        serde_json::from_str(&metadata).map_err(|err| format!("地图元数据不是有效 JSON：{err}"))?;
    let labels = metadata
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut response = operator_status(endpoint)?;
    response.summary = format!("控制服务在线，已加载 {} 个地点", labels.len());
    response.labels = Some(labels);
    if let Ok(recording) = request(endpoint, "GET", "/viz/api/dynamic-map/recording", None)
        && let Ok(recording) = serde_json::from_str::<Value>(&recording)
    {
        response.recording_enabled = recording.get("enabled").and_then(Value::as_bool);
    }
    Ok(response)
}

fn operator_status(endpoint: &ControlEndpoint) -> Result<ResponseData, String> {
    let body = request(endpoint, "GET", "/viz/api/map/metadata", None)?;
    let _: Value =
        serde_json::from_str(&body).map_err(|err| format!("地图状态不是有效 JSON：{err}"))?;
    Ok(ResponseData {
        summary: "机器人控制服务在线".to_owned(),
        ..Default::default()
    })
}

fn request(
    endpoint: &ControlEndpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let address = format!("{}:{}", endpoint.host, endpoint.port);
    let socket = address
        .to_socket_addrs()
        .map_err(|err| format!("无法解析 {address}：{err}"))?
        .next()
        .ok_or_else(|| format!("无法解析 {address}"))?;

    let mut stream =
        TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT).map_err(|err| match err.kind() {
            std::io::ErrorKind::ConnectionRefused => {
                "机器人控制服务拒绝连接，请检查地址和服务状态".to_owned()
            }
            std::io::ErrorKind::TimedOut => "连接机器人控制服务超时，请检查网络".to_owned(),
            _ => format!("无法连接机器人控制服务：{err}"),
        })?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();

    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        endpoint.host,
        endpoint.port,
        body.len(),
        body,
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("发送控制请求失败：{err}"))?;

    let mut bytes = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("读取控制响应失败：{err}"))?;
    let response = String::from_utf8_lossy(&bytes);
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "控制服务返回了无效 HTTP 响应".to_owned())?;
    let status = headers.lines().next().unwrap_or_default();
    let status_code = status
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status_code) {
        let detail = summarize_json(body);
        return Err(if status_code == 502 {
            format!("机器人服务不可用：{detail}")
        } else {
            format!("请求失败（HTTP {status_code}）：{detail}")
        });
    }
    Ok(body.to_owned())
}

fn summarize_json(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(180).collect();
    };
    for key in ["message", "detail", "status"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return text.to_owned();
        }
    }
    body.chars().take(180).collect()
}
