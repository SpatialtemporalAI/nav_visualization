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

    pub fn replay_url(&self, task_id: &str) -> String {
        format!(
            "{}/viz/api/replay/tasks/{}/recording.rrd",
            self.display_url(),
            percent_encode_path_segment(task_id)
        )
    }
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Refresh,
    Status,
    ReplayTasks,
    Navigate,
    Stop,
    Recording,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Refresh => "刷新状态",
            Self::Status => "同步状态",
            Self::ReplayTasks => "加载回放",
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
    LoadReplayTasks,
    Navigate { goal_text: String, dry_run: bool },
    Stop,
    SetRecording(bool),
}

impl ControlCommand {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::Refresh => ActionKind::Refresh,
            Self::Status => ActionKind::Status,
            Self::LoadReplayTasks => ActionKind::ReplayTasks,
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
    pub operator_status: Option<OperatorStatus>,
    pub navigation_running: Option<bool>,
    pub replay_tasks: Option<Vec<ReplayTask>>,
}

#[derive(Debug, Default)]
pub struct OperatorStatus {
    pub schema_version: String,
    pub task_id: Option<String>,
    pub status: String,
    pub goal_text: Option<String>,
    pub navigation_running: bool,
    pub upstream_connected: bool,
    pub upstream_error: Option<String>,
    pub last_upstream_message_at: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ReplayTask {
    pub task_id: String,
    pub goal_text: Option<String>,
    pub status: String,
    pub has_rerun_recording: bool,
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
        ControlCommand::LoadReplayTasks => replay_tasks(endpoint),
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
    Ok(response)
}

fn operator_status(endpoint: &ControlEndpoint) -> Result<ResponseData, String> {
    let body = request(endpoint, "GET", "/viz/api/operator/status", None)?;
    let value: Value =
        serde_json::from_str(&body).map_err(|err| format!("操作状态不是有效 JSON：{err}"))?;
    let task = value
        .get("task")
        .ok_or_else(|| "操作状态缺少 task 字段".to_owned())?;
    let status = OperatorStatus {
        schema_version: value
            .get("schema_version")
            .and_then(Value::as_str)
            .unwrap_or("—")
            .to_owned(),
        task_id: task
            .get("task_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        status: task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("idle")
            .to_owned(),
        goal_text: task
            .get("goal_text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        navigation_running: value
            .get("navigation_running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        upstream_connected: value
            .get("upstream_connected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        upstream_error: value
            .get("upstream_error")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        last_upstream_message_at: value
            .get("last_upstream_message_at")
            .and_then(Value::as_f64),
    };
    Ok(ResponseData {
        summary: "操作状态已同步".to_owned(),
        recording_enabled: value.get("dynamic_map_recording").and_then(Value::as_bool),
        operator_status: Some(status),
        ..Default::default()
    })
}

fn replay_tasks(endpoint: &ControlEndpoint) -> Result<ResponseData, String> {
    let body = request(endpoint, "GET", "/viz/api/replay/tasks?limit=20", None)?;
    let value: Value =
        serde_json::from_str(&body).map_err(|err| format!("回放列表不是有效 JSON：{err}"))?;
    let tasks = value
        .as_array()
        .ok_or_else(|| "回放列表不是数组".to_owned())?
        .iter()
        .filter_map(|item| {
            Some(ReplayTask {
                task_id: item.get("task_id")?.as_str()?.to_owned(),
                goal_text: item
                    .get("goal_text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                status: item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                has_rerun_recording: item
                    .get("has_rerun_recording")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    let available = tasks.iter().filter(|task| task.has_rerun_recording).count();
    Ok(ResponseData {
        summary: format!("已找到 {available} 个 Rerun 任务回放"),
        replay_tasks: Some(tasks),
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
                "后台服务尚未就绪，首次启动可能需要几分钟".to_owned()
            }
            std::io::ErrorKind::TimedOut => "连接后台服务超时，请稍后重试".to_owned(),
            _ => format!("无法连接后台服务：{err}"),
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
