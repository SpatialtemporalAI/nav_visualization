use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tungstenite::{Error as WsError, Message};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const RERUN_START_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct NativeSidecarSettings {
    pub robot_ip: String,
    pub robot_port: u16,
    pub rerun_port: u16,
    pub history_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct NativeStatusSnapshot {
    pub schema_version: String,
    pub task_id: Option<String>,
    pub task_status: String,
    pub goal_text: Option<String>,
    pub navigation_running: bool,
    pub recording_enabled: bool,
    pub robot_pose: Option<NativePoseSnapshot>,
    pub vpr_pose: Option<NativePoseSnapshot>,
    pub goal_pose: Option<NativePoseSnapshot>,
    pub planner_mode: Option<String>,
    pub map_revision: Option<u64>,
    pub dynamic_revision: Option<u64>,
    pub path_points: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct NativePoseSnapshot {
    pub x: f64,
    pub y: f64,
    pub theta: f64,
}

#[derive(Clone, Debug)]
pub struct NativeReplayTask {
    pub task_id: String,
    pub goal_text: Option<String>,
    pub status: String,
    pub has_rerun_recording: bool,
    pub recording_source: String,
    pub task_dir: PathBuf,
}

impl Default for NativeStatusSnapshot {
    fn default() -> Self {
        Self {
            schema_version: "—".to_owned(),
            task_id: None,
            task_status: "idle".to_owned(),
            goal_text: None,
            navigation_running: false,
            recording_enabled: false,
            robot_pose: None,
            vpr_pose: None,
            goal_pose: None,
            planner_mode: None,
            map_revision: None,
            dynamic_revision: None,
            path_points: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct MapMetadata {
    origin: [f64; 2],
    resolution: f64,
    height: f64,
}

pub struct NativeSidecar {
    stop: Arc<AtomicBool>,
    reconnect: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    last_message_ms: Arc<AtomicU64>,
    status: Arc<Mutex<NativeStatusSnapshot>>,
    settings: Arc<Mutex<NativeSidecarSettings>>,
    thread: Option<JoinHandle<()>>,
    errors: mpsc::Receiver<String>,
}

impl NativeSidecar {
    pub fn start(settings: NativeSidecarSettings) -> Result<Self, String> {
        let rerun_port = settings.rerun_port;
        let stop = Arc::new(AtomicBool::new(false));
        let reconnect = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let last_message_ms = Arc::new(AtomicU64::new(0));
        let status = Arc::new(Mutex::new(NativeStatusSnapshot::default()));
        let settings = Arc::new(Mutex::new(settings));
        let worker_stop = Arc::clone(&stop);
        let worker_reconnect = Arc::clone(&reconnect);
        let worker_connected = Arc::clone(&connected);
        let worker_last_message_ms = Arc::clone(&last_message_ms);
        let worker_status = Arc::clone(&status);
        let worker_settings = Arc::clone(&settings);
        let (error_tx, errors) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("woosh-native-sidecar".to_owned())
            .spawn(move || {
                if let Err(err) = run_worker(
                    &worker_settings,
                    &worker_stop,
                    &worker_reconnect,
                    &worker_connected,
                    &worker_last_message_ms,
                    &worker_status,
                ) {
                    eprintln!("内置数据服务退出：{err}");
                    let _ = error_tx.send(err);
                }
            })
            .map_err(|err| format!("无法启动内置数据服务：{err}"))?;
        let mut worker = Self {
            stop,
            reconnect,
            connected,
            last_message_ms,
            status,
            settings,
            thread: Some(thread),
            errors,
        };
        let address = SocketAddr::from(([127, 0, 0, 1], rerun_port));
        let deadline = Instant::now() + RERUN_START_TIMEOUT;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
                return Ok(worker);
            }
            if let Some(err) = worker.poll_error() {
                worker.stop();
                return Err(err);
            }
            thread::sleep(Duration::from_millis(20));
        }
        worker.stop();
        Err(format!("内置 Rerun 服务未能在 {} 端口及时启动", rerun_port))
    }

    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    pub fn poll_error(&mut self) -> Option<String> {
        self.errors.try_recv().ok()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn last_message_at(&self) -> Option<f64> {
        let milliseconds = self.last_message_ms.load(Ordering::Relaxed);
        (milliseconds != 0).then_some(milliseconds as f64 / 1_000.0)
    }

    pub fn status(&self) -> NativeStatusSnapshot {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    pub fn rerun_port(&self) -> Option<u16> {
        self.settings
            .lock()
            .ok()
            .map(|settings| settings.rerun_port)
    }

    pub fn reconfigure(&self, settings: NativeSidecarSettings) -> Result<(), String> {
        let mut current = self
            .settings
            .lock()
            .map_err(|_| "无法更新内置数据服务设置".to_owned())?;
        if current.rerun_port != settings.rerun_port {
            return Err("本机 Rerun 端口已变化，需要重启数据服务".to_owned());
        }
        *current = settings;
        drop(current);
        self.connected.store(false, Ordering::Relaxed);
        self.last_message_ms.store(0, Ordering::Relaxed);
        if let Ok(mut status) = self.status.lock() {
            *status = NativeStatusSnapshot::default();
        }
        self.reconnect.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for NativeSidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(
    settings: &Mutex<NativeSidecarSettings>,
    stop: &AtomicBool,
    reconnect: &AtomicBool,
    connected: &AtomicBool,
    last_message_ms: &AtomicU64,
    status: &Mutex<NativeStatusSnapshot>,
) -> Result<(), String> {
    let initial_settings = settings
        .lock()
        .map_err(|_| "无法读取内置数据服务设置".to_owned())?
        .clone();
    let rec = rerun::RecordingStreamBuilder::new("woosh_robot_navigation")
        .serve_grpc_opts(
            "127.0.0.1",
            initial_settings.rerun_port,
            rerun::ServerOptions {
                memory_limit: rerun::MemoryLimit::parse("256MiB")
                    .unwrap_or(rerun::MemoryLimit::UNLIMITED),
                ..Default::default()
            },
        )
        .map_err(|err| format!("无法启动内置 Rerun 服务：{err}"))?;
    send_default_blueprint(&rec);

    let mut logger = NavigationLogger::new(rec, initial_settings);
    let mut retry_delay = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let active_settings = settings
            .lock()
            .map_err(|_| "无法读取内置数据服务设置".to_owned())?
            .clone();
        logger.update_settings(active_settings.clone());
        reconnect.store(false, Ordering::Relaxed);
        match consume_websocket(
            &active_settings,
            stop,
            reconnect,
            connected,
            last_message_ms,
            status,
            &mut logger,
        ) {
            Ok(()) if stop.load(Ordering::Relaxed) => break,
            Ok(()) => {}
            Err(err) => {
                connected.store(false, Ordering::Relaxed);
                let _ =
                    logger.log_event("events/connection", &format!("上游连接中断：{err}"), "WARN");
            }
        }
        if reconnect.load(Ordering::Relaxed) {
            retry_delay = Duration::from_secs(1);
            continue;
        }
        let deadline = Instant::now() + retry_delay;
        while !stop.load(Ordering::Relaxed)
            && !reconnect.load(Ordering::Relaxed)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(100));
        }
        retry_delay = (retry_delay * 2).min(Duration::from_secs(10));
    }
    connected.store(false, Ordering::Relaxed);
    Ok(())
}

fn send_default_blueprint(rec: &rerun::RecordingStream) {
    use rerun::blueprint::components::PanelState;
    use rerun::blueprint::{
        Blueprint, BlueprintActivation, BlueprintPanel, Horizontal, SelectionPanel, Spatial2DView,
        Tabs, TextDocumentView, TextLogView, TimePanel, Vertical,
    };

    let cameras = Tabs::new([
        Spatial2DView::new("Front Camera")
            .with_origin("sensors/front/rgb")
            .into(),
        Spatial2DView::new("NavDP Plan")
            .with_origin("planner/navdp/input")
            .into(),
    ]);
    let overview = Horizontal::new([
        Spatial2DView::new("Map & Path").with_origin("world").into(),
        cameras.into(),
    ])
    .with_column_shares(vec![1.0, 1.0]);
    let details = Horizontal::new([
        TextLogView::new("Task Events").with_origin("events").into(),
        TextDocumentView::new("Live Data")
            .with_origin("status/live")
            .into(),
    ])
    .with_column_shares(vec![3.2, 1.0]);
    let layout = Vertical::new([overview.into(), details.into()]).with_row_shares(vec![1.75, 1.0]);
    let blueprint = Blueprint::new(layout)
        .with_auto_views(false)
        .with_auto_layout(false)
        .with_blueprint_panel(BlueprintPanel::from_state(PanelState::Collapsed))
        .with_selection_panel(SelectionPanel::from_state(PanelState::Collapsed))
        .with_time_panel(TimePanel::new().with_state(PanelState::Collapsed));
    let _ = blueprint.send(rec, BlueprintActivation::default());
}

fn consume_websocket(
    settings: &NativeSidecarSettings,
    stop: &AtomicBool,
    reconnect: &AtomicBool,
    connected: &AtomicBool,
    last_message_ms: &AtomicU64,
    status: &Mutex<NativeStatusSnapshot>,
    logger: &mut NavigationLogger,
) -> Result<(), String> {
    let address = format!("{}:{}", settings.robot_ip, settings.robot_port);
    let socket = address
        .to_socket_addrs()
        .map_err(|err| format!("无法解析机器人地址 {address}：{err}"))?
        .next()
        .ok_or_else(|| format!("无法解析机器人地址 {address}"))?;
    let stream = TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT)
        .map_err(|err| format!("无法连接机器人数据流：{err}"))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|err| format!("无法设置数据流超时：{err}"))?;
    let url = format!("ws://{}:{}/viz/ws", settings.robot_ip, settings.robot_port);
    let (mut websocket, _) =
        tungstenite::client(url, stream).map_err(|err| format!("WebSocket 握手失败：{err}"))?;

    connected.store(true, Ordering::Relaxed);
    logger.refresh_map()?;
    let mut last_map_poll = Instant::now();
    while !stop.load(Ordering::Relaxed) && !reconnect.load(Ordering::Relaxed) {
        match websocket.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|err| format!("数据流 JSON 无效：{err}"))?;
                last_message_ms.store(now_millis(), Ordering::Relaxed);
                update_native_status(status, &value);
                logger.log_message(&value);
                if let Ok(status) = status.lock() {
                    logger.log_status_document(&status);
                }
            }
            Ok(Message::Binary(_)) => {}
            Ok(Message::Ping(payload)) => {
                let _ = websocket.send(Message::Pong(payload));
            }
            Ok(Message::Close(_)) => return Err("机器人关闭了数据流".to_owned()),
            Ok(_) => {}
            Err(WsError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(err) => return Err(err.to_string()),
        }
        if last_map_poll.elapsed() >= Duration::from_secs(5) {
            let _ = logger.refresh_map();
            last_map_poll = Instant::now();
        }
    }
    let _ = websocket.close(None);
    Ok(())
}

fn update_native_status(status: &Mutex<NativeStatusSnapshot>, message: &Value) {
    let Ok(mut status) = status.lock() else {
        return;
    };
    if let Some(version) = message.get("schema_version").and_then(Value::as_str) {
        status.schema_version = version.to_owned();
    }
    let message_type = message.get("type").and_then(Value::as_str).unwrap_or("");
    match message_type {
        "snapshot" => {
            status.robot_pose = parse_pose_snapshot(message.pointer("/robot/pose"));
            status.vpr_pose = parse_pose_snapshot(message.pointer("/robot/vpr_pose"));
            status.goal_pose = parse_pose_snapshot(message.pointer("/goal/pose"));
            status.planner_mode = message
                .pointer("/planner/mode")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            status.path_points = message
                .pointer("/planner/global_path")
                .and_then(Value::as_array)
                .map(Vec::len);
            update_map_status(&mut status, message.get("dynamic_map"));
        }
        "pose_update" => {
            if message.get("pose").is_some() {
                status.robot_pose = parse_pose_snapshot(message.get("pose"));
            }
            if message.get("vpr_pose").is_some() {
                status.vpr_pose = parse_pose_snapshot(message.get("vpr_pose"));
            }
        }
        "goal_update" => {
            if message.pointer("/goal/pose").is_some() {
                status.goal_pose = parse_pose_snapshot(message.pointer("/goal/pose"));
            }
        }
        "planner_update" => {
            if message.get("mode").is_some() {
                status.planner_mode = message
                    .get("mode")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            if message.get("global_path").is_some() {
                status.path_points = message
                    .get("global_path")
                    .and_then(Value::as_array)
                    .map(Vec::len);
            }
        }
        "dynamic_map_update" => update_map_status(&mut status, Some(message)),
        _ => {}
    }
    let task = if message_type == "snapshot" {
        message.get("task")
    } else if message_type == "task_status" {
        Some(message)
    } else {
        None
    };
    if let Some(task) = task {
        if task.get("task_id").is_some() {
            status.task_id = task
                .get("task_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if let Some(value) = task.get("status").and_then(Value::as_str) {
            status.task_status = value.to_lowercase();
        }
        if task.get("goal_text").is_some() {
            status.goal_text = task
                .get("goal_text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        status.navigation_running = matches!(
            status.task_status.as_str(),
            "accepted"
                | "queued"
                | "running"
                | "planning"
                | "navigating"
                | "executing"
                | "processing"
                | "busy"
        );
    }
    if message_type == "snapshot" {
        status.recording_enabled = message
            .get("dynamic_map_recording")
            .and_then(Value::as_bool)
            .unwrap_or(status.recording_enabled);
    } else if message_type == "dynamic_map_recording_update" {
        status.recording_enabled = message
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(status.recording_enabled);
    }
}

fn parse_pose_snapshot(value: Option<&Value>) -> Option<NativePoseSnapshot> {
    let value = value.filter(|value| !value.is_null())?;
    Some(NativePoseSnapshot {
        x: value.get("x")?.as_f64()?,
        y: value.get("y")?.as_f64()?,
        theta: value.get("theta").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

fn update_map_status(status: &mut NativeStatusSnapshot, value: Option<&Value>) {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return;
    };
    if value.get("map_revision").is_some() {
        status.map_revision = value.get("map_revision").and_then(Value::as_u64);
    }
    if value.get("revision").is_some() {
        status.dynamic_revision = value.get("revision").and_then(Value::as_u64);
    }
}

struct NavigationLogger {
    rec: rerun::RecordingStream,
    task_rec: Option<rerun::RecordingStream>,
    active_task_id: Option<String>,
    active_task_dir: Option<PathBuf>,
    settings: NativeSidecarSettings,
    map: MapMetadata,
    map_signature: String,
    map_image: Option<Vec<u8>>,
    images: HashMap<String, String>,
    status_signature: String,
}

impl NavigationLogger {
    fn new(rec: rerun::RecordingStream, settings: NativeSidecarSettings) -> Self {
        Self {
            rec,
            task_rec: None,
            active_task_id: None,
            active_task_dir: None,
            settings,
            map: MapMetadata {
                resolution: 1.0,
                ..Default::default()
            },
            map_signature: String::new(),
            map_image: None,
            images: HashMap::new(),
            status_signature: String::new(),
        }
    }

    fn update_settings(&mut self, settings: NativeSidecarSettings) {
        if self.settings.robot_ip == settings.robot_ip
            && self.settings.robot_port == settings.robot_port
        {
            self.settings = settings;
            return;
        }
        self.settings = settings;
        self.task_rec = None;
        self.active_task_id = None;
        self.active_task_dir = None;
        self.map = MapMetadata {
            resolution: 1.0,
            ..Default::default()
        };
        self.map_signature.clear();
        self.map_image = None;
        self.images.clear();
        self.status_signature.clear();
        self.rec
            .set_timestamp_secs_since_epoch("navigation_time", now_secs());
        for entity in [
            "world/robot",
            "world/vpr_pose",
            "world/goal",
            "world/planner",
            "world/dynamic/occupancy",
            "sensors",
            "planner",
        ] {
            let _ = self.rec.log(entity, &rerun::Clear::recursive());
        }
    }

    fn base_url(&self) -> String {
        format!(
            "http://{}:{}",
            self.settings.robot_ip, self.settings.robot_port
        )
    }

    fn refresh_map(&mut self) -> Result<(), String> {
        let url = format!("{}/viz/api/map/metadata", self.base_url());
        let mut response = ureq::get(&url)
            .call()
            .map_err(|err| format!("读取地图元数据失败：{err}"))?;
        let metadata: Value = response
            .body_mut()
            .read_json()
            .map_err(|err| format!("地图元数据 JSON 无效：{err}"))?;
        let signature = metadata.to_string();
        if signature == self.map_signature {
            return Ok(());
        }
        self.map = MapMetadata {
            origin: [
                metadata
                    .pointer("/origin/0")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                metadata
                    .pointer("/origin/1")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            ],
            resolution: metadata
                .get("resolution")
                .and_then(Value::as_f64)
                .filter(|value| *value > 0.0)
                .unwrap_or(1.0),
            height: metadata
                .get("height")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        };
        if metadata
            .get("image_path")
            .is_some_and(|value| !value.is_null())
        {
            let image_url = format!("{}/viz/api/map/image", self.base_url());
            if let Ok(mut image_response) = ureq::get(&image_url).call()
                && let Ok(bytes) = image_response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_to_vec()
            {
                self.map_image = Some(bytes.clone());
                let image = rerun::EncodedImage::from_file_contents(bytes)
                    .with_media_type("image/png")
                    .with_draw_order(-100.0);
                self.log_static("world/map", &image);
            }
        }
        let annotation = rerun::AnnotationContext::new([
            (0, "Free", rerun::Rgba32::from_unmultiplied_rgba(0, 0, 0, 0)),
            (
                1,
                "Dynamic Inflation",
                rerun::Rgba32::from_unmultiplied_rgba(214, 172, 70, 155),
            ),
            (
                2,
                "Dynamic Obstacle",
                rerun::Rgba32::from_unmultiplied_rgba(245, 82, 65, 225),
            ),
        ]);
        self.log_static("world/dynamic", &annotation);
        self.map_signature = signature;
        Ok(())
    }

    fn log_message(&mut self, message: &Value) {
        let finish_recording = self.prepare_task_recording(message);
        self.set_time(message);
        match message.get("type").and_then(Value::as_str).unwrap_or("") {
            "snapshot" => self.log_snapshot(message),
            "pose_update" => {
                self.log_pose(
                    "world/robot",
                    message.get("pose"),
                    [42, 203, 225, 255],
                    "Robot 定位",
                    true,
                    30.0,
                );
                self.log_pose(
                    "world/vpr_pose",
                    message.get("vpr_pose"),
                    [167, 139, 250, 255],
                    "VPR 定位",
                    false,
                    26.0,
                );
            }
            "goal_update" => self.log_pose(
                "world/goal",
                message.pointer("/goal/pose"),
                [255, 113, 91, 255],
                "导航目标",
                true,
                28.0,
            ),
            "planner_update" => self.log_planner(message),
            "image_update" => self.log_images(message.get("images")),
            "dynamic_map_update" => self.log_dynamic_map(message),
            "task_status" => {
                let _ = self.log_event("events/task_status", &message.to_string(), "INFO");
            }
            "event" => {
                let level = message
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or("INFO");
                let _ = self.log_event("events/navigation", &message.to_string(), level);
            }
            _ => {}
        }
        if finish_recording {
            self.finish_task_recording();
        }
    }

    fn log_snapshot(&mut self, message: &Value) {
        let synthetic_task = json!({
            "type": "task_status",
            "schema_version": message.get("schema_version"),
            "timestamp": message.get("timestamp"),
            "task_id": message.pointer("/task/task_id"),
            "status": message.pointer("/task/status"),
            "goal_text": message.pointer("/task/goal_text"),
        });
        self.log_message(&synthetic_task);
        let synthetic_pose = json!({
            "type": "pose_update",
            "timestamp": message.get("timestamp"),
            "pose": message.pointer("/robot/pose"),
            "vpr_pose": message.pointer("/robot/vpr_pose"),
        });
        self.log_message(&synthetic_pose);
        let synthetic_goal = json!({
            "type": "goal_update",
            "timestamp": message.get("timestamp"),
            "goal": message.get("goal"),
        });
        self.log_message(&synthetic_goal);
        if let Some(planner) = message.get("planner") {
            let mut planner = planner.clone();
            if let Some(object) = planner.as_object_mut() {
                object.insert(
                    "type".to_owned(),
                    Value::String("planner_update".to_owned()),
                );
            }
            self.log_message(&planner);
        }
        self.log_images(message.get("images"));
        if let Some(dynamic) = message.get("dynamic_map") {
            self.log_dynamic_map(dynamic);
        }
    }

    fn set_time(&self, _message: &Value) {
        // Use arrival time for the live timeline. Robot clocks can differ between
        // addresses; using their raw timestamps can place a newly selected robot
        // behind the previous stream and make its latest-at data appear missing.
        let timestamp = now_secs();
        self.rec
            .set_timestamp_secs_since_epoch("navigation_time", timestamp);
        if let Some(task) = &self.task_rec {
            task.set_timestamp_secs_since_epoch("navigation_time", timestamp);
        }
    }

    fn point_to_pixel(&self, point: [f64; 2]) -> [f32; 2] {
        [
            ((point[0] - self.map.origin[0]) / self.map.resolution) as f32,
            (self.map.height - (point[1] - self.map.origin[1]) / self.map.resolution) as f32,
        ]
    }

    fn log_pose(
        &self,
        entity: &str,
        pose: Option<&Value>,
        color: [u8; 4],
        label: &str,
        heading: bool,
        draw_order: f32,
    ) {
        let Some(pose) = pose.filter(|pose| !pose.is_null()) else {
            self.log(entity, &rerun::Clear::recursive());
            return;
        };
        let Some(x) = pose.get("x").and_then(Value::as_f64) else {
            return;
        };
        let Some(y) = pose.get("y").and_then(Value::as_f64) else {
            return;
        };
        let theta = pose.get("theta").and_then(Value::as_f64).unwrap_or(0.0);
        let point = self.point_to_pixel([x, y]);
        let (radii, colors): (Vec<f32>, Vec<[u8; 4]>) = if heading {
            (
                vec![9.0, 7.3, 2.7],
                vec![[12, 12, 12, 245], color, [244, 249, 252, 245]],
            )
        } else {
            (
                vec![6.2, 5.0, 1.9],
                vec![[12, 12, 12, 245], color, [244, 249, 252, 245]],
            )
        };
        let hover = format!(
            "{label}  ·  x {x:.2} m  ·  y {y:.2} m  ·  朝向 {:.1}°",
            theta.to_degrees()
        );

        let points = rerun::Points2D::new(vec![point; radii.len()])
            .with_colors(colors)
            .with_radii(radii.into_iter().map(rerun::Radius::new_ui_points))
            .with_labels(vec![hover.as_str(); 3])
            .with_show_labels(false)
            .with_draw_order(draw_order);
        self.log(entity, &points);

        if heading {
            let forward = [theta.cos() as f32, -theta.sin() as f32];
            let arrow = rerun::Arrows2D::from_vectors([[forward[0] * 15.0, forward[1] * 15.0]])
                .with_origins([point])
                .with_colors([color])
                .with_radii([rerun::Radius::new_ui_points(2.0)])
                .with_labels([hover.as_str()])
                .with_show_labels(false)
                .with_draw_order(draw_order + 4.0);
            self.log(entity, &arrow);
        }
    }

    fn log_status_document(&mut self, status: &NativeStatusSnapshot) {
        let pose = |pose: Option<&NativePoseSnapshot>| {
            pose.map(|pose| format!("{:.2}, {:.2} m · {:.2} rad", pose.x, pose.y, pose.theta))
                .unwrap_or_else(|| "—".to_owned())
        };
        let revision = match (status.map_revision, status.dynamic_revision) {
            (Some(map), Some(dynamic)) => format!("{map} / {dynamic}"),
            (Some(map), None) => format!("{map} / —"),
            (None, Some(dynamic)) => format!("— / {dynamic}"),
            (None, None) => "—".to_owned(),
        };
        let document = format!(
            "**Robot**  \n{}\n\n**VPR**  \n{}\n\n**Goal**  \n{}\n\n---\n\n**Mode**　{}  \n**Map**　{}  \n**Path**　{} points",
            pose(status.robot_pose.as_ref()),
            pose(status.vpr_pose.as_ref()),
            pose(status.goal_pose.as_ref()),
            status.planner_mode.as_deref().unwrap_or("—"),
            revision,
            status.path_points.unwrap_or(0),
        );
        if self.status_signature == document {
            return;
        }
        self.status_signature.clone_from(&document);
        self.log("status/live", &rerun::TextDocument::from_markdown(document));
    }

    fn log_planner(&self, message: &Value) {
        self.log_path(
            "world/planner/global_path",
            message.get("global_path"),
            [77, 222, 155, 255],
            6.0,
        );
        self.log_path(
            "world/planner/local_path",
            message.get("local_path"),
            [255, 191, 71, 255],
            7.0,
        );
        self.log_points(
            "world/planner/waypoints",
            message.get("waypoints"),
            [167, 139, 250, 255],
        );
        if let Some(goal) = message.get("local_goal") {
            self.log_points(
                "world/planner/local_goal",
                Some(&Value::Array(vec![goal.clone()])),
                [255, 220, 92, 255],
            );
        }
    }

    fn log_path(&self, entity: &str, value: Option<&Value>, color: [u8; 4], draw_order: f32) {
        let points = self.parse_points(value);
        if points.len() < 2 {
            self.log(entity, &rerun::Clear::recursive());
            return;
        }
        let path = rerun::LineStrips2D::new([points])
            .with_colors([color])
            .with_radii([rerun::Radius::new_ui_points(2.5)])
            .with_draw_order(draw_order);
        self.log(entity, &path);
    }

    fn log_points(&self, entity: &str, value: Option<&Value>, color: [u8; 4]) {
        let points = self.parse_points(value);
        if points.is_empty() {
            self.log(entity, &rerun::Clear::recursive());
            return;
        }
        let points = rerun::Points2D::new(points)
            .with_colors([color])
            .with_radii([rerun::Radius::new_ui_points(4.5)])
            .with_draw_order(8.0);
        self.log(entity, &points);
    }

    fn parse_points(&self, value: Option<&Value>) -> Vec<[f32; 2]> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|point| {
                let point = point.as_array()?;
                Some(self.point_to_pixel([point.first()?.as_f64()?, point.get(1)?.as_f64()?]))
            })
            .collect()
    }

    fn log_images(&mut self, images: Option<&Value>) {
        let Some(images) = images.and_then(Value::as_object) else {
            return;
        };
        for (key, descriptor) in images {
            let Some(url) = descriptor.get("url").and_then(Value::as_str) else {
                continue;
            };
            let identity = format!(
                "{}:{url}",
                descriptor.get("version").unwrap_or(&Value::Null)
            );
            if self.images.get(key) == Some(&identity) {
                continue;
            }
            let full_url = if url.starts_with("http://") || url.starts_with("https://") {
                url.to_owned()
            } else {
                format!(
                    "{}{}{}",
                    self.base_url(),
                    if url.starts_with('/') { "" } else { "/" },
                    url
                )
            };
            let Ok(mut response) = ureq::get(&full_url).call() else {
                continue;
            };
            let Ok(bytes) = response
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_vec()
            else {
                continue;
            };
            let entity = match key.as_str() {
                "rgb_latest" => "sensors/front/rgb".to_owned(),
                "rgb_navdp" => "planner/navdp/input".to_owned(),
                _ => format!("sensors/{key}"),
            };
            self.log(&entity, &rerun::EncodedImage::from_file_contents(bytes));
            self.images.insert(key.clone(), identity);
        }
    }

    fn log_dynamic_map(&self, message: &Value) {
        if message
            .get("clear")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.log("world/dynamic/occupancy", &rerun::Clear::recursive());
            return;
        }
        let width = message.get("width").and_then(Value::as_u64).unwrap_or(0) as usize;
        let height = message.get("height").and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(length) = width.checked_mul(height).filter(|length| *length > 0) else {
            return;
        };
        let mut mask = vec![0_u8; length];
        apply_runs(&mut mask, message.get("inflated_runs"), 1);
        apply_runs(&mut mask, message.get("occupied_runs"), 2);
        let Ok(mask) = ndarray::Array2::from_shape_vec((height, width), mask) else {
            return;
        };
        let Ok(image) = rerun::SegmentationImage::try_from(mask) else {
            return;
        };
        let image = image.with_opacity(0.55).with_draw_order(1.0);
        self.log("world/dynamic/occupancy", &image);
    }

    fn log_event(&self, entity: &str, text: &str, level: &str) -> Result<(), String> {
        let value = rerun::TextLog::new(text).with_level(level.to_uppercase());
        self.log(entity, &value);
        Ok(())
    }

    fn log(&self, entity: &str, value: &impl rerun::AsComponents) {
        let _ = self.rec.log(entity, value);
        if let Some(task) = &self.task_rec {
            let _ = task.log(entity, value);
        }
    }

    fn log_static(&self, entity: &str, value: &impl rerun::AsComponents) {
        let _ = self.rec.log_static(entity, value);
        if let Some(task) = &self.task_rec {
            let _ = task.log_static(entity, value);
        }
    }

    fn prepare_task_recording(&mut self, message: &Value) -> bool {
        if message.get("type").and_then(Value::as_str) != Some("task_status") {
            return false;
        }
        let task_id = message.get("task_id").and_then(Value::as_str);
        let status = message
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("idle")
            .to_lowercase();
        let active = matches!(
            status.as_str(),
            "accepted"
                | "queued"
                | "running"
                | "planning"
                | "navigating"
                | "executing"
                | "processing"
                | "busy"
        );
        let terminal = matches!(
            status.as_str(),
            "succeeded"
                | "success"
                | "completed"
                | "failed"
                | "cancelled"
                | "canceled"
                | "stopped"
                | "aborted"
        );
        if active && task_id.is_some() && self.active_task_id.as_deref() != task_id {
            self.finish_task_recording();
            self.start_task_recording(
                task_id.unwrap_or_default(),
                message.get("goal_text").and_then(Value::as_str),
                &status,
            );
        }
        if self.active_task_id.as_deref() == task_id {
            self.write_task_metadata(message.get("goal_text").and_then(Value::as_str), &status);
        }
        terminal && self.active_task_id.as_deref() == task_id
    }

    fn start_task_recording(&mut self, task_id: &str, goal_text: Option<&str>, status: &str) {
        let safe_id = task_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .take(80)
            .collect::<String>();
        let task_dir = self
            .settings
            .history_dir
            .join(format!("{}-{safe_id}", now_millis()));
        if std::fs::create_dir_all(&task_dir).is_err() {
            return;
        }
        let recording_path = task_dir.join("recording.rrd");
        let Ok(task_rec) =
            rerun::RecordingStreamBuilder::new("woosh_robot_navigation").save(&recording_path)
        else {
            return;
        };
        send_default_blueprint(&task_rec);
        self.task_rec = Some(task_rec);
        self.active_task_id = Some(task_id.to_owned());
        self.active_task_dir = Some(task_dir);
        self.write_task_metadata(goal_text, status);
        if let Some(bytes) = &self.map_image {
            let image = rerun::EncodedImage::from_file_contents(bytes.clone())
                .with_media_type("image/png")
                .with_draw_order(-100.0);
            if let Some(task) = &self.task_rec {
                let _ = task.log_static("world/map", &image);
            }
        }
        self.log_dynamic_annotation_to_task();
    }

    fn write_task_metadata(&self, goal_text: Option<&str>, status: &str) {
        let (Some(task_dir), Some(task_id)) = (&self.active_task_dir, &self.active_task_id) else {
            return;
        };
        let metadata = json!({
            "task_id": task_id,
            "goal_text": goal_text,
            "status": status,
            "updated_at": now_secs(),
        });
        let _ = std::fs::write(task_dir.join("task.json"), metadata.to_string());
    }

    fn log_dynamic_annotation_to_task(&self) {
        let Some(task) = &self.task_rec else { return };
        let annotation = rerun::AnnotationContext::new([
            (0, "Free", rerun::Rgba32::from_unmultiplied_rgba(0, 0, 0, 0)),
            (
                1,
                "Dynamic Inflation",
                rerun::Rgba32::from_unmultiplied_rgba(214, 172, 70, 155),
            ),
            (
                2,
                "Dynamic Obstacle",
                rerun::Rgba32::from_unmultiplied_rgba(245, 82, 65, 225),
            ),
        ]);
        let _ = task.log_static("world/dynamic", &annotation);
    }

    fn finish_task_recording(&mut self) {
        if let Some(task) = self.task_rec.take() {
            let _ = task.flush_blocking();
        }
        self.active_task_id = None;
        self.active_task_dir = None;
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn apply_runs(mask: &mut [u8], runs: Option<&Value>, class_id: u8) {
    let Some(runs) = runs.and_then(Value::as_array) else {
        return;
    };
    for run in runs {
        let Some(run) = run.as_array() else { continue };
        let start = run.first().and_then(Value::as_u64).unwrap_or(0) as usize;
        let length = run.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = start.saturating_add(length).min(mask.len());
        if start < end {
            mask[start..end].fill(class_id);
        }
    }
}

pub fn replay_tasks_in(history_dir: &std::path::Path) -> Vec<NativeReplayTask> {
    let Ok(entries) = std::fs::read_dir(history_dir) else {
        return Vec::new();
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH),
        )
    });
    entries
        .into_iter()
        .take(50)
        .filter_map(|entry| {
            let task_dir = entry.path();
            let recording = task_dir.join("recording.rrd");
            if !recording.is_file() {
                return None;
            }
            let metadata = std::fs::read_to_string(task_dir.join("task.json"))
                .ok()
                .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
                .unwrap_or(Value::Null);
            let fallback_id = task_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("task")
                .to_owned();
            Some(NativeReplayTask {
                task_id: metadata
                    .get("task_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&fallback_id)
                    .to_owned(),
                goal_text: metadata
                    .get("goal_text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                status: metadata
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("recorded")
                    .to_owned(),
                has_rerun_recording: true,
                recording_source: recording.to_string_lossy().into_owned(),
                task_dir,
            })
        })
        .collect()
}

pub fn delete_replay_task(
    history_dir: &std::path::Path,
    task_dir: &std::path::Path,
) -> Result<(), String> {
    let history_dir = history_dir
        .canonicalize()
        .map_err(|err| format!("无法访问任务记录目录 {}：{err}", history_dir.display()))?;
    let task_dir = task_dir
        .canonicalize()
        .map_err(|err| format!("无法访问任务记录 {}：{err}", task_dir.display()))?;
    if task_dir.parent() != Some(history_dir.as_path()) || !task_dir.join("recording.rrd").is_file()
    {
        return Err("拒绝删除：所选目录不是有效的任务记录".to_owned());
    }
    std::fs::remove_dir_all(&task_dir)
        .map_err(|err| format!("无法删除任务记录 {}：{err}", task_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_task_deletion_is_limited_to_a_direct_history_child() {
        let root = std::env::temp_dir().join(format!(
            "woosh-viewer-delete-test-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let history = root.join("rerun-history");
        let task = history.join("task-1");
        let outside = root.join("outside");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(task.join("recording.rrd"), b"test").unwrap();
        std::fs::write(outside.join("recording.rrd"), b"test").unwrap();

        assert!(delete_replay_task(&history, &outside).is_err());
        assert!(outside.is_dir());
        delete_replay_task(&history, &task).unwrap();
        assert!(!task.exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn start_returns_only_after_rerun_server_is_reachable() {
        let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let rerun_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let mut sidecar = NativeSidecar::start(NativeSidecarSettings {
            robot_ip: "127.0.0.1".to_owned(),
            robot_port: 1,
            rerun_port,
            history_dir: std::env::temp_dir().join("woosh-viewer-test-history"),
        })
        .unwrap();

        assert!(TcpStream::connect(("127.0.0.1", rerun_port)).is_ok());
        sidecar.stop();
    }

    #[test]
    fn websocket_snapshot_populates_realtime_metrics_and_running_state() {
        let status = Mutex::new(NativeStatusSnapshot::default());
        update_native_status(
            &status,
            &json!({
                "type": "snapshot",
                "schema_version": "v1",
                "task": {
                    "task_id": "task-1",
                    "goal_text": "去电梯门口",
                    "status": "processing"
                },
                "robot": {
                    "pose": {"x": 8.027, "y": 3.818, "theta": 1.2},
                    "vpr_pose": {"x": 7.902, "y": 4.207, "theta": 1.1}
                },
                "goal": {"pose": {"x": -0.57, "y": -0.77, "theta": -1.56}},
                "planner": {
                    "mode": "line_segment",
                    "global_path": [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]
                },
                "dynamic_map": {"map_revision": 3373033343885312_u64, "revision": 13},
                "dynamic_map_recording": true
            }),
        );

        let status = status.lock().unwrap();
        assert!(status.navigation_running);
        assert_eq!(status.task_status, "processing");
        assert_eq!(status.planner_mode.as_deref(), Some("line_segment"));
        assert_eq!(status.path_points, Some(3));
        assert_eq!(status.map_revision, Some(3373033343885312));
        assert_eq!(status.dynamic_revision, Some(13));
        assert!((status.robot_pose.as_ref().unwrap().x - 8.027).abs() < f64::EPSILON);
        assert!((status.vpr_pose.as_ref().unwrap().y - 4.207).abs() < f64::EPSILON);
        assert!((status.goal_pose.as_ref().unwrap().theta + 1.56).abs() < f64::EPSILON);
    }

    #[test]
    fn incremental_websocket_updates_replace_visible_metrics() {
        let status = Mutex::new(NativeStatusSnapshot::default());
        update_native_status(
            &status,
            &json!({
                "type": "pose_update",
                "pose": {"x": 1.0, "y": 2.0, "theta": 0.5},
                "vpr_pose": {"x": 1.1, "y": 2.1, "theta": 0.6}
            }),
        );
        update_native_status(
            &status,
            &json!({
                "type": "goal_update",
                "goal": {"pose": {"x": 3.0, "y": 4.0, "theta": 1.5}}
            }),
        );
        update_native_status(
            &status,
            &json!({
                "type": "planner_update",
                "mode": "waypoint",
                "global_path": [[1.0, 2.0], [3.0, 4.0]]
            }),
        );
        update_native_status(
            &status,
            &json!({
                "type": "dynamic_map_update",
                "map_revision": 22,
                "revision": 7
            }),
        );
        update_native_status(&status, &json!({"type": "task_status", "status": "busy"}));

        let status = status.lock().unwrap();
        assert!(status.navigation_running);
        assert_eq!(status.planner_mode.as_deref(), Some("waypoint"));
        assert_eq!(status.path_points, Some(2));
        assert_eq!(status.map_revision, Some(22));
        assert_eq!(status.dynamic_revision, Some(7));
        assert_eq!(status.goal_pose.as_ref().unwrap().x, 3.0);
    }

    #[test]
    fn terminal_task_status_clears_running_state() {
        let status = Mutex::new(NativeStatusSnapshot::default());
        update_native_status(
            &status,
            &json!({"type": "task_status", "status": "processing"}),
        );
        update_native_status(
            &status,
            &json!({"type": "task_status", "status": "stopped"}),
        );

        assert!(!status.lock().unwrap().navigation_running);
    }
}
