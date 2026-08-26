mod control_client;

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use control_client::{
    ActionKind, ControlClient, ControlCommand, ControlEndpoint, OperatorStatus, ReplayTask,
};
use rerun::external::{eframe, egui, re_crash_handler, re_log, re_memory, re_viewer, tokio};
use serde::{Deserialize, Serialize};

#[global_allocator]
static GLOBAL: re_memory::AccountingAllocator<mimalloc::MiMalloc> =
    re_memory::AccountingAllocator::new(mimalloc::MiMalloc);

#[derive(Clone, Debug)]
struct ViewerConfig {
    robot_ip: String,
    robot_port: u16,
    control_port: u16,
    rerun_port: u16,
    rerun_url: Option<String>,
    screenshot: Option<PathBuf>,
    config_path: PathBuf,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            robot_ip: "192.168.123.161".to_owned(),
            robot_port: 8008,
            control_port: 8010,
            rerun_port: 9876,
            rerun_url: None,
            screenshot: None,
            config_path: default_config_write_path(),
        }
    }
}

impl ViewerConfig {
    fn parse() -> Result<Self, String> {
        let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
        let explicit_config = raw_args
            .iter()
            .position(|arg| arg == "--config")
            .map(|index| {
                raw_args
                    .get(index + 1)
                    .map(PathBuf::from)
                    .ok_or_else(|| "--config 缺少参数".to_owned())
            })
            .transpose()?;

        let config_path = explicit_config.clone().or_else(default_config_path);
        let mut config = if let Some(path) = &config_path {
            Self::from_file(path, explicit_config.is_some())?
        } else {
            Self::default()
        };
        config.config_path = config_path.unwrap_or_else(default_config_write_path);
        let mut args = raw_args.into_iter();
        while let Some(arg) = args.next() {
            let value = |args: &mut std::vec::IntoIter<String>, flag: &str| {
                args.next().ok_or_else(|| format!("{flag} 缺少参数"))
            };
            match arg.as_str() {
                "--config" => {
                    let _ = value(&mut args, &arg)?;
                }
                "--robot-ip" => config.robot_ip = value(&mut args, &arg)?,
                "--robot-port" => {
                    config.robot_port = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "--robot-port 必须是有效端口".to_owned())?;
                }
                "--control-port" => {
                    config.control_port = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "--control-port 必须是有效端口".to_owned())?;
                }
                "--rerun-port" => {
                    config.rerun_port = value(&mut args, &arg)?
                        .parse()
                        .map_err(|_| "--rerun-port 必须是有效端口".to_owned())?;
                }
                "--rerun-url" => config.rerun_url = Some(value(&mut args, &arg)?),
                "--screenshot" => {
                    config.screenshot = Some(PathBuf::from(value(&mut args, &arg)?));
                }
                "--help" | "-h" => {
                    println!(
                        "woosh-viewer [--config FILE] [--robot-ip IP] [--robot-port PORT] [--control-port PORT] \
                         [--rerun-port PORT] [--rerun-url URL] [--screenshot FILE]"
                    );
                    std::process::exit(0);
                }
                _ => return Err(format!("未知参数：{arg}")),
            }
        }
        Ok(config)
    }

    fn from_file(path: &std::path::Path, required: bool) -> Result<Self, String> {
        if !path.is_file() {
            return if required {
                Err(format!("配置文件不存在：{}", path.display()))
            } else {
                Ok(Self::default())
            };
        }
        let contents = std::fs::read_to_string(path)
            .map_err(|err| format!("无法读取配置文件 {}：{err}", path.display()))?;
        let file: FileConfig = toml::from_str(&contents)
            .map_err(|err| format!("配置文件 {} 格式错误：{err}", path.display()))?;
        let mut config = Self::default();
        if let Some(robot_ip) = file.robot_ip {
            config.robot_ip = robot_ip;
        }
        if let Some(robot_port) = file.robot_port {
            config.robot_port = robot_port;
        }
        if let Some(control_port) = file.control_port {
            config.control_port = control_port;
        }
        if let Some(rerun_port) = file.rerun_port {
            config.rerun_port = rerun_port;
        }
        config.rerun_url = file.rerun_url;
        Ok(config)
    }

    fn rerun_url(&self) -> String {
        self.rerun_url
            .clone()
            .unwrap_or_else(|| format!("rerun+http://127.0.0.1:{}/proxy", self.rerun_port))
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    robot_ip: Option<String>,
    robot_port: Option<u16>,
    control_port: Option<u16>,
    rerun_port: Option<u16>,
    rerun_url: Option<String>,
}

fn default_config_path() -> Option<PathBuf> {
    let beside_executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("woosh-viewer.toml")));
    beside_executable.filter(|path| path.is_file()).or_else(|| {
        let path = PathBuf::from("woosh-viewer.toml");
        path.is_file().then_some(path)
    })
}

fn default_config_write_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("woosh-viewer.toml")))
        .unwrap_or_else(|| PathBuf::from("woosh-viewer.toml"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveConnection {
    control_port: u16,
    rerun_port: u16,
}

impl LiveConnection {
    fn from_config(config: &ViewerConfig) -> Self {
        Self {
            control_port: config.control_port,
            rerun_port: config.rerun_port,
        }
    }

    fn control_endpoint(&self) -> ControlEndpoint {
        ControlEndpoint {
            host: "127.0.0.1".to_owned(),
            port: self.control_port,
        }
    }

    fn rerun_url(&self) -> String {
        format!("rerun+http://127.0.0.1:{}/proxy", self.rerun_port)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidecarSettings {
    robot_ip: String,
    robot_port: u16,
    control_port: u16,
    rerun_port: u16,
}

impl SidecarSettings {
    fn from_config(config: &ViewerConfig) -> Result<Self, String> {
        Self::from_input(
            &config.robot_ip,
            config.robot_port,
            config.control_port,
            config.rerun_port,
        )
    }

    fn from_input(
        robot_ip: &str,
        robot_port: u16,
        control_port: u16,
        rerun_port: u16,
    ) -> Result<Self, String> {
        let robot_ip = robot_ip.trim();
        if robot_ip.is_empty() {
            return Err("机器人 IP / 主机名不能为空".to_owned());
        }
        if robot_ip.contains("://")
            || robot_ip.contains('/')
            || robot_ip.chars().any(char::is_whitespace)
        {
            return Err("机器人地址只填写 IP 或主机名，不要包含 http://、端口或路径".to_owned());
        }
        if robot_port == 0 || control_port == 0 || rerun_port == 0 {
            return Err("端口必须在 1–65535 之间".to_owned());
        }
        Ok(Self {
            robot_ip: robot_ip.to_owned(),
            robot_port,
            control_port,
            rerun_port,
        })
    }

    fn connection(&self) -> LiveConnection {
        LiveConnection {
            control_port: self.control_port,
            rerun_port: self.rerun_port,
        }
    }
}

struct ConnectionRequest {
    settings: SidecarSettings,
    connection: LiveConnection,
    save: bool,
}

fn connection_config_contents(settings: &SidecarSettings) -> Result<String, String> {
    toml::to_string_pretty(&FileConfig {
        robot_ip: Some(settings.robot_ip.clone()),
        robot_port: Some(settings.robot_port),
        control_port: Some(settings.control_port),
        rerun_port: Some(settings.rerun_port),
        rerun_url: None,
    })
    .map_err(|err| format!("无法生成配置：{err}"))
}

fn save_connection_config(
    path: &std::path::Path,
    settings: &SidecarSettings,
) -> Result<(), String> {
    let contents = connection_config_contents(settings)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("无法创建配置目录 {}：{err}", parent.display()))?;
    }
    std::fs::write(path, contents).map_err(|err| format!("无法保存配置 {}：{err}", path.display()))
}

struct SidecarManager {
    child: Option<Child>,
    log_path: PathBuf,
}

impl SidecarManager {
    fn new(log_path: PathBuf) -> Self {
        Self {
            child: None,
            log_path,
        }
    }

    fn restart(&mut self, settings: &SidecarSettings) -> Result<(), String> {
        self.stop();
        let script = find_sidecar_launcher().ok_or_else(|| {
            "找不到 run-sidecar-windows.ps1；请保留完整的 woosh-windows 目录结构".to_owned()
        })?;

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let log = std::fs::File::create(&self.log_path).map_err(|err| {
                format!("无法创建 sidecar 日志 {}：{err}", self.log_path.display())
            })?;
            let error_log = log
                .try_clone()
                .map_err(|err| format!("无法打开 sidecar 日志：{err}"))?;
            let child = Command::new("powershell.exe")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&script)
                .arg("-RobotIp")
                .arg(&settings.robot_ip)
                .arg("-RobotPort")
                .arg(settings.robot_port.to_string())
                .arg("-ControlPort")
                .arg(settings.control_port.to_string())
                .arg("-RerunPort")
                .arg(settings.rerun_port.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(error_log))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|err| format!("无法启动 sidecar：{err}"))?;
            self.child = Some(child);
            Ok(())
        }

        #[cfg(not(windows))]
        {
            let _ = (settings, script);
            Err("自动启动 sidecar 当前仅支持 Windows".to_owned())
        }
    }

    fn poll_exit(&mut self) -> Option<String> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                let detail = std::fs::read_to_string(&self.log_path)
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .rev()
                            .find(|line| !line.trim().is_empty())
                            .map(str::to_owned)
                    });
                Some(match detail {
                    Some(detail) => format!(
                        "sidecar 已退出（{status}）：{detail}。日志：{}",
                        self.log_path.display()
                    ),
                    None => format!(
                        "sidecar 已退出（{status}）。日志：{}",
                        self.log_path.display()
                    ),
                })
            }
            Ok(None) => None,
            Err(err) => Some(format!("无法读取 sidecar 状态：{err}")),
        }
    }

    fn is_running(&self) -> bool {
        self.child.is_some()
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .output();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.stop();
    }
}

fn find_sidecar_launcher() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("run-sidecar-windows.ps1"));
        candidates.push(directory.join(r"..\..\run-sidecar-windows.ps1"));
    }
    candidates.push(PathBuf::from("run-sidecar-windows.ps1"));
    candidates.into_iter().find(|path| path.is_file())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ViewerConfig::parse().map_err(|err| {
        eprintln!("参数错误：{err}");
        err
    })?;
    let main_thread_token = re_viewer::MainThreadToken::i_promise_i_am_on_the_main_thread();
    re_log::setup_logging();
    re_crash_handler::install_crash_handlers(re_viewer::build_info());

    let mut native_options = re_viewer::native::eframe_options(None);
    native_options.viewport = native_options
        .viewport
        .with_app_id("woosh_robot_operator")
        .with_inner_size([1600.0, 920.0])
        .with_min_inner_size([1100.0, 680.0]);

    let mut startup_options = re_viewer::StartupOptions::default();
    startup_options.detach_process = false;
    startup_options.expect_data_soon = Some(true);
    startup_options.screenshot_to_path_then_quit = config.screenshot.clone();

    let rerun_url = config.rerun_url();
    let live_connection = LiveConnection::from_config(&config);
    let sidecar_settings = SidecarSettings::from_config(&config)?;
    let control_endpoint = live_connection.control_endpoint();
    let rerun_port = config.rerun_port;
    let config_path = config.config_path.clone();

    eframe::run_native(
        "Woosh Robot Operator",
        native_options,
        Box::new(move |cc| {
            re_viewer::customize_eframe_and_setup_renderer(cc)?;
            let rerun_app = re_viewer::App::new(
                main_thread_token,
                re_viewer::build_info(),
                re_viewer::AppEnvironment::Custom("Woosh Robot Operator".to_owned()),
                startup_options,
                cc,
                None,
                re_viewer::AsyncRuntimeHandle::from_current_tokio_runtime_or_wasmbindgen()?,
            );
            rerun_app.open_url_or_file(&rerun_url);

            let mut app = WooshApp {
                rerun_app,
                rerun_url,
                controls: ControlPanel::new(
                    control_endpoint,
                    rerun_port,
                    config_path,
                    sidecar_settings,
                ),
                cjk_font: load_cjk_font(),
            };
            app.controls.refresh(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )?;
    Ok(())
}

struct WooshApp {
    rerun_app: re_viewer::App,
    rerun_url: String,
    controls: ControlPanel,
    cjk_font: Option<Vec<u8>>,
}

impl eframe::App for WooshApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.rerun_app.save(storage);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(font) = self.cjk_font.take() {
            install_cjk_font(ui.ctx(), font);
        }
        let telemetry_loaded = self.rerun_app.recording_db().is_some();
        let reconnect = egui::Panel::left("woosh_control_panel")
            .default_size(350.0)
            .min_size(320.0)
            .max_size(440.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.controls.ui(ui, telemetry_loaded, &self.rerun_url)
                    })
                    .inner
            })
            .inner;
        if let Some(request) = self.controls.take_requested_connection() {
            let save_result = request
                .save
                .then(|| save_connection_config(self.controls.config_path(), &request.settings));
            self.rerun_url = request.connection.rerun_url();
            self.controls.apply_connection(
                request.settings,
                request.connection,
                save_result,
                ui.ctx().clone(),
            );
            self.rerun_app.open_url_or_file(&self.rerun_url);
        } else if reconnect {
            self.rerun_app.open_url_or_file(&self.rerun_url);
        }
        if let Some(source) = self.controls.take_requested_source() {
            self.rerun_app.open_url_or_file(&source);
        }
        self.rerun_app.ui(ui, frame);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.controls.poll_results();
        self.controls.poll_status(ctx.clone());
        ctx.request_repaint_after(Duration::from_millis(250));
        self.rerun_app.logic(ctx, frame);
    }
}

fn load_cjk_font() -> Option<Vec<u8>> {
    let mut candidates = Vec::new();
    if let Ok(windows_dir) = std::env::var("WINDIR") {
        let fonts = PathBuf::from(windows_dir).join("Fonts");
        candidates.extend([
            fonts.join("msyh.ttc"),
            fonts.join("msyhbd.ttc"),
            fonts.join("simsun.ttc"),
        ]);
    }
    candidates.extend([
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf"),
        PathBuf::from("/System/Library/Fonts/PingFang.ttc"),
    ]);
    candidates
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
}

fn install_cjk_font(ctx: &egui::Context, font: Vec<u8>) {
    let mut definitions = ctx.fonts(|fonts| fonts.definitions().clone());
    let name = "woosh-cjk".to_owned();
    definitions.font_data.insert(
        name.clone(),
        std::sync::Arc::new(egui::FontData::from_owned(font)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(fonts) = definitions.families.get_mut(&family) {
            fonts.push(name.clone());
        }
    }
    ctx.set_fonts(definitions);
}

struct ControlPanel {
    client: ControlClient,
    sidecar: SidecarManager,
    config_path: PathBuf,
    connection_open: bool,
    advanced_connection_open: bool,
    diagnostics_open: bool,
    connection_robot_ip: String,
    connection_robot_port: u16,
    connection_control_port: u16,
    connection_rerun_port: u16,
    connection_save: bool,
    connection_notice: Option<(String, bool)>,
    requested_connection: Option<ConnectionRequest>,
    goal_text: String,
    dry_run: bool,
    labels: Vec<String>,
    recording_enabled: bool,
    control_online: bool,
    upstream_connected: bool,
    upstream_error: Option<String>,
    last_upstream_message_at: Option<f64>,
    schema_version: String,
    task_id: Option<String>,
    task_status: String,
    task_goal: Option<String>,
    navigation_running: bool,
    last_status_poll: Instant,
    replay_open: bool,
    replay_loaded: bool,
    replay_tasks: Vec<ReplayTask>,
    replay_error: Option<String>,
    selected_replay: Option<String>,
    source_label: String,
    requested_source: Option<String>,
    pending: HashSet<ActionKind>,
    message: String,
    message_is_error: bool,
}

impl ControlPanel {
    fn new(
        endpoint: ControlEndpoint,
        rerun_port: u16,
        config_path: PathBuf,
        settings: SidecarSettings,
    ) -> Self {
        let connection_control_port = endpoint.port;
        let log_path = config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("woosh-sidecar.log");
        let mut sidecar = SidecarManager::new(log_path);
        let start_result = sidecar.restart(&settings);
        let mut panel = Self {
            client: ControlClient::new(endpoint),
            sidecar,
            config_path,
            connection_open: false,
            advanced_connection_open: false,
            diagnostics_open: false,
            connection_robot_ip: settings.robot_ip,
            connection_robot_port: settings.robot_port,
            connection_control_port,
            connection_rerun_port: rerun_port,
            connection_save: true,
            connection_notice: None,
            requested_connection: None,
            goal_text: String::new(),
            dry_run: false,
            labels: Vec::new(),
            recording_enabled: false,
            control_online: false,
            upstream_connected: false,
            upstream_error: None,
            last_upstream_message_at: None,
            schema_version: "—".to_owned(),
            task_id: None,
            task_status: "idle".to_owned(),
            task_goal: None,
            navigation_running: false,
            last_status_poll: Instant::now(),
            replay_open: false,
            replay_loaded: false,
            replay_tasks: Vec::new(),
            replay_error: None,
            selected_replay: None,
            source_label: "实时数据".to_owned(),
            requested_source: None,
            pending: HashSet::new(),
            message: "正在启动 sidecar 并连接机器人…".to_owned(),
            message_is_error: false,
        };
        if let Err(err) = start_result {
            panel.message = err.clone();
            panel.message_is_error = true;
            panel.connection_notice = Some((err, true));
            panel.connection_open = true;
        }
        panel
    }

    fn refresh(&mut self, ctx: egui::Context) {
        self.dispatch(ControlCommand::Refresh, ctx);
    }

    fn data_age_s(&self) -> Option<f64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        self.last_upstream_message_at
            .map(|timestamp| (now - timestamp).max(0.0))
    }

    fn data_is_live(&self) -> bool {
        self.upstream_connected && self.data_age_s().is_some_and(|age| age <= 10.0)
    }

    fn navigation_ready(&self) -> bool {
        self.control_online && self.data_is_live()
    }

    fn startup_label(&self) -> &'static str {
        if !self.sidecar.is_running() {
            "正在启动后台服务"
        } else if !self.control_online {
            "正在准备运行环境"
        } else if !self.upstream_connected {
            "正在连接机器人"
        } else if !self.data_is_live() {
            "正在等待实时数据"
        } else {
            "已就绪"
        }
    }

    fn connection_guidance(&self) -> String {
        if !self.sidecar.is_running() {
            return "后台服务未运行，请展开“机器人连接”后重新启动".to_owned();
        }
        if !self.control_online {
            return "后台正在准备环境，首次启动可能需要几分钟".to_owned();
        }
        if !self.upstream_connected {
            return format!(
                "无法连接机器人 {}:{}，请检查 IP、网络和机器人服务",
                self.connection_robot_ip, self.connection_robot_port
            );
        }
        if !self.data_is_live() {
            return "机器人连接存在，但实时数据已中断，请尝试重新连接".to_owned();
        }
        "实时画面正在加载，请稍候".to_owned()
    }

    fn dispatch(&mut self, command: ControlCommand, ctx: egui::Context) {
        let kind = command.kind();
        self.pending.insert(kind);
        if !matches!(kind, ActionKind::Status) {
            self.message = format!("{}中…", kind.label());
            self.message_is_error = false;
        }
        self.client.dispatch(command, ctx);
    }

    fn poll_status(&mut self, ctx: egui::Context) {
        if self.pending.contains(&ActionKind::Status)
            || self.last_status_poll.elapsed() < Duration::from_secs(1)
        {
            return;
        }
        self.last_status_poll = Instant::now();
        self.dispatch(ControlCommand::Status, ctx);
    }

    fn take_requested_source(&mut self) -> Option<String> {
        self.requested_source.take()
    }

    fn config_path(&self) -> &std::path::Path {
        &self.config_path
    }

    fn take_requested_connection(&mut self) -> Option<ConnectionRequest> {
        self.requested_connection.take()
    }

    fn apply_connection(
        &mut self,
        settings: SidecarSettings,
        connection: LiveConnection,
        save_result: Option<Result<(), String>>,
        ctx: egui::Context,
    ) {
        let sidecar_result = self.sidecar.restart(&settings);
        self.client.set_endpoint(connection.control_endpoint());
        self.pending.clear();
        self.control_online = false;
        self.upstream_connected = false;
        self.upstream_error = None;
        self.last_upstream_message_at = None;
        self.labels.clear();
        self.schema_version = "—".to_owned();
        self.task_id = None;
        self.task_status = "idle".to_owned();
        self.task_goal = None;
        self.navigation_running = false;
        self.recording_enabled = false;
        self.replay_loaded = false;
        self.replay_tasks.clear();
        self.replay_error = None;
        self.selected_replay = None;
        self.source_label = "实时数据".to_owned();
        self.message = match &sidecar_result {
            Ok(()) => format!(
                "正在连接机器人 {}:{}…",
                settings.robot_ip, settings.robot_port
            ),
            Err(err) => err.clone(),
        };
        self.message_is_error = sidecar_result.is_err();
        self.connection_notice = if let Err(err) = sidecar_result {
            Some((err, true))
        } else {
            match save_result {
                Some(Ok(())) => Some((
                    format!(
                        "sidecar 已启动，配置已保存到 {}",
                        self.config_path.display()
                    ),
                    false,
                )),
                Some(Err(err)) => Some((format!("sidecar 已启动，但{err}"), true)),
                None => Some(("sidecar 已启动（配置未保存）".to_owned(), false)),
            }
        };
        self.last_status_poll = Instant::now();
        self.refresh(ctx);
    }

    fn poll_results(&mut self) {
        if let Some(err) = self.sidecar.poll_exit() {
            self.message = err.clone();
            self.message_is_error = true;
            self.connection_notice = Some((err, true));
            self.control_online = false;
            self.upstream_connected = false;
        }
        while let Some(event) = self.client.try_recv() {
            if event.generation != self.client.generation() {
                continue;
            }
            self.pending.remove(&event.kind);
            match event.result {
                Ok(data) => {
                    self.control_online = true;
                    if !matches!(event.kind, ActionKind::Status) {
                        self.message = data.summary;
                        self.message_is_error = data.summary_is_error;
                    }
                    if let Some(labels) = data.labels {
                        self.labels = labels;
                    }
                    if let Some(enabled) = data.recording_enabled {
                        self.recording_enabled = enabled;
                    }
                    if let Some(status) = data.operator_status {
                        self.apply_operator_status(status);
                    }
                    if let Some(running) = data.navigation_running {
                        self.navigation_running = running;
                    }
                    if let Some(tasks) = data.replay_tasks {
                        self.replay_tasks = tasks;
                        self.replay_loaded = true;
                        self.replay_error = None;
                    }
                }
                Err(err) => match event.kind {
                    ActionKind::Status => self.control_online = false,
                    ActionKind::Refresh => {
                        self.control_online = false;
                        self.message = err;
                        self.message_is_error = true;
                    }
                    ActionKind::ReplayTasks => {
                        self.replay_loaded = true;
                        self.replay_error = Some(err);
                    }
                    _ => {
                        self.message = err;
                        self.message_is_error = true;
                    }
                },
            }
        }
    }

    fn apply_operator_status(&mut self, status: OperatorStatus) {
        self.schema_version = status.schema_version;
        self.task_id = status.task_id;
        self.task_status = status.status;
        self.task_goal = status.goal_text;
        self.navigation_running = status.navigation_running;
        self.upstream_connected = status.upstream_connected;
        self.upstream_error = status.upstream_error;
        self.last_upstream_message_at = status.last_upstream_message_at;
    }

    fn connection_ui(&mut self, ui: &mut egui::Ui) {
        let title = format!(
            "机器人连接（{}:{}）",
            self.connection_robot_ip, self.connection_robot_port
        );
        if section_toggle(ui, &title, self.connection_open) {
            self.connection_open = !self.connection_open;
        }
        if !self.connection_open {
            return;
        }

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label("机器人 IP / 主机名");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.connection_robot_ip)
                        .hint_text("例如 192.168.4.38")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.connection_notice = None;
            }
            ui.add_space(4.0);
            if section_toggle(ui, "高级端口设置", self.advanced_connection_open) {
                self.advanced_connection_open = !self.advanced_connection_open;
            }
            if self.advanced_connection_open {
                egui::Grid::new("woosh_connection_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("机器人端口");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.connection_robot_port)
                                    .range(1..=u16::MAX),
                            )
                            .changed()
                        {
                            self.connection_notice = None;
                        }
                        ui.end_row();
                        ui.label("控制端口");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.connection_control_port)
                                    .range(1..=u16::MAX),
                            )
                            .changed()
                        {
                            self.connection_notice = None;
                        }
                        ui.end_row();
                        ui.label("Rerun 端口");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.connection_rerun_port)
                                    .range(1..=u16::MAX),
                            )
                            .changed()
                        {
                            self.connection_notice = None;
                        }
                        ui.end_row();
                    });
            }
            ui.small("后台会自动连接机器人；首次启动可能需要几分钟。");
            ui.checkbox(&mut self.connection_save, "保存到 woosh-viewer.toml");
            if ui
                .add(
                    egui::Button::new("启动 sidecar 并连接")
                        .min_size(egui::vec2(ui.available_width(), 32.0)),
                )
                .clicked()
            {
                match SidecarSettings::from_input(
                    &self.connection_robot_ip,
                    self.connection_robot_port,
                    self.connection_control_port,
                    self.connection_rerun_port,
                ) {
                    Ok(settings) => {
                        self.connection_robot_ip = settings.robot_ip.clone();
                        let connection = settings.connection();
                        self.requested_connection = Some(ConnectionRequest {
                            settings,
                            connection,
                            save: self.connection_save,
                        });
                        self.connection_notice = Some(("正在启动 sidecar…".to_owned(), false));
                    }
                    Err(err) => self.connection_notice = Some((err, true)),
                }
            }
            if let Some((notice, is_error)) = &self.connection_notice {
                let color = if *is_error {
                    egui::Color32::from_rgb(235, 95, 105)
                } else {
                    egui::Color32::from_rgb(110, 205, 165)
                };
                ui.label(egui::RichText::new(notice).color(color));
            }
            ui.small(format!("配置文件：{}", self.config_path.display()));
        });
    }

    fn replay_ui(&mut self, ui: &mut egui::Ui, rerun_url: &str) {
        let replay_count = self
            .replay_tasks
            .iter()
            .filter(|task| task.has_rerun_recording)
            .count();
        let title = if self.replay_loaded {
            format!("本机任务记录（{replay_count}）")
        } else {
            "本机任务记录".to_owned()
        };
        if section_toggle(ui, &title, self.replay_open) {
            self.replay_open = !self.replay_open;
            if self.replay_open
                && !self.replay_loaded
                && !self.pending.contains(&ActionKind::ReplayTasks)
            {
                self.dispatch(ControlCommand::LoadReplayTasks, ui.ctx().clone());
            }
        }
        if !self.replay_open {
            return;
        }

        let mut open_task = None;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.selected_replay.is_none(), "实时数据")
                    .clicked()
                {
                    self.selected_replay = None;
                    self.source_label = "实时数据".to_owned();
                    self.requested_source = Some(rerun_url.to_owned());
                }
                let loading = self.pending.contains(&ActionKind::ReplayTasks);
                if ui
                    .add_enabled(!loading, egui::Button::new("刷新"))
                    .clicked()
                {
                    self.replay_loaded = false;
                    self.replay_error = None;
                    self.dispatch(ControlCommand::LoadReplayTasks, ui.ctx().clone());
                }
                if loading {
                    ui.spinner();
                }
            });
            ui.small("这些记录保存在当前电脑；选择后可在右侧时间轴中播放。");
            if let Some(error) = &self.replay_error {
                ui.colored_label(egui::Color32::from_rgb(235, 95, 105), error);
            }
            for task in self
                .replay_tasks
                .iter()
                .filter(|task| task.has_rerun_recording)
                .take(12)
            {
                let short_id = task.task_id.chars().take(8).collect::<String>();
                let goal = task.goal_text.as_deref().unwrap_or("未命名任务");
                let label = format!("{goal}\n{short_id} · {}", task.status);
                if ui
                    .selectable_label(
                        self.selected_replay.as_deref() == Some(task.task_id.as_str()),
                        label,
                    )
                    .clicked()
                {
                    open_task = Some((task.task_id.clone(), goal.to_owned()));
                }
            }
            if self.replay_loaded && replay_count == 0 && self.replay_error.is_none() {
                ui.weak("暂无带 Rerun 录制的历史任务");
            }
        });
        if let Some((task_id, goal)) = open_task {
            self.requested_source = Some(self.client.endpoint().replay_url(&task_id));
            self.selected_replay = Some(task_id);
            self.source_label = format!("回放：{goal}");
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, telemetry_loaded: bool, rerun_url: &str) -> bool {
        let mut reconnect = false;
        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            ui.heading(egui::RichText::new("WOOSH").size(23.0).strong());
            ui.label(egui::RichText::new("ROBOT OPERATOR").size(11.0).weak());
        });
        ui.horizontal(|ui| {
            ui.small("主题");
            egui::global_theme_preference_buttons(ui);
        });
        ui.add_space(8.0);

        self.connection_ui(ui);
        ui.add_space(8.0);

        let ready = self.navigation_ready() && telemetry_loaded;
        let stage_color = if ready {
            egui::Color32::from_rgb(70, 210, 145)
        } else if self.message_is_error {
            egui::Color32::from_rgb(235, 95, 105)
        } else {
            egui::Color32::from_rgb(245, 166, 35)
        };
        ui.horizontal(|ui| {
            if !ready && !self.message_is_error {
                ui.spinner();
            }
            ui.label(
                egui::RichText::new(self.startup_label())
                    .color(stage_color)
                    .strong(),
            );
        });
        ui.add_space(6.0);
        ui.strong("运行状态");
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                status_dot(ui, self.sidecar.is_running());
                ui.strong("后台服务");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(if self.sidecar.is_running() {
                        if self.control_online {
                            "运行中"
                        } else {
                            "准备中"
                        }
                    } else {
                        "未运行"
                    });
                });
            });
            ui.horizontal(|ui| {
                status_dot(ui, self.upstream_connected);
                ui.strong("机器人连接");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(if self.upstream_connected {
                        "已连接"
                    } else {
                        "未连接"
                    });
                });
            });
            ui.horizontal(|ui| {
                status_dot(ui, self.data_is_live() && telemetry_loaded);
                ui.strong("实时画面与地图");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match self.data_age_s() {
                        Some(age) if age <= 2.0 && telemetry_loaded => "实时",
                        Some(age) if age <= 10.0 => "略有延迟",
                        Some(_) => "数据中断",
                        None => "正在等待",
                    };
                    ui.label(label);
                });
            });
            ui.horizontal(|ui| {
                status_dot(ui, self.navigation_running);
                ui.strong("当前导航");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(task_status_label(&self.task_status));
                });
            });
            if let Some(goal) = &self.task_goal {
                ui.add_space(4.0);
                ui.label(format!("目标：{goal}"));
            }
        });

        if !self.navigation_ready() {
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(245, 166, 35),
                self.connection_guidance(),
            );
        }

        ui.add_space(10.0);
        ui.strong("发送导航任务");
        ui.small("选择地点或直接描述机器人要去的位置");
        ui.add_space(6.0);
        ui.add_enabled_ui(self.navigation_ready(), |ui| {
            if !self.labels.is_empty() {
                egui::ComboBox::from_id_salt("woosh_known_goal")
                    .selected_text(if self.goal_text.is_empty() {
                        "选择已知地点"
                    } else {
                        &self.goal_text
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for label in &self.labels {
                            ui.selectable_value(&mut self.goal_text, label.clone(), label);
                        }
                    });
                ui.add_space(4.0);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.goal_text)
                    .hint_text("例如：去电梯门口")
                    .desired_width(f32::INFINITY),
            );
            ui.checkbox(&mut self.dry_run, "仅模拟路线，不让机器人移动");

            let navigate_pending = self.pending.contains(&ActionKind::Navigate);
            let can_navigate =
                !self.goal_text.trim().is_empty() && !navigate_pending && !self.navigation_running;
            let navigate_text = if navigate_pending {
                "正在提交…"
            } else if self.dry_run {
                "开始模拟规划"
            } else {
                "开始导航"
            };
            if ui
                .add_enabled(
                    can_navigate,
                    egui::Button::new(egui::RichText::new(navigate_text).strong())
                        .fill(egui::Color32::from_rgb(36, 103, 210))
                        .min_size(egui::vec2(ui.available_width(), 38.0)),
                )
                .clicked()
            {
                self.dispatch(
                    ControlCommand::Navigate {
                        goal_text: self.goal_text.trim().to_owned(),
                        dry_run: self.dry_run,
                    },
                    ui.ctx().clone(),
                );
            }

            if self.navigation_running {
                ui.add_space(5.0);
                let stop_pending = self.pending.contains(&ActionKind::Stop);
                let stop_button = egui::Button::new(
                    egui::RichText::new(if stop_pending {
                        "正在停止…"
                    } else {
                        "停止当前导航"
                    })
                    .color(egui::Color32::WHITE)
                    .strong(),
                )
                .fill(egui::Color32::from_rgb(180, 48, 58))
                .min_size(egui::vec2(ui.available_width(), 38.0));
                if ui.add_enabled(!stop_pending, stop_button).clicked() {
                    self.dispatch(ControlCommand::Stop, ui.ctx().clone());
                }
            }

            ui.add_space(10.0);
            let mut recording = self.recording_enabled;
            if ui
                .checkbox(&mut recording, "保存本次任务的地图变化")
                .changed()
            {
                self.recording_enabled = recording;
                self.dispatch(ControlCommand::SetRecording(recording), ui.ctx().clone());
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.strong("回放与工具");
        ui.add_space(4.0);
        self.replay_ui(ui, rerun_url);
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong(if self.message_is_error {
                "需要处理"
            } else {
                "操作提示"
            });
            ui.add_space(3.0);
            let color = if self.message_is_error {
                egui::Color32::from_rgb(235, 95, 105)
            } else {
                egui::Color32::from_rgb(110, 205, 165)
            };
            ui.label(egui::RichText::new(&self.message).color(color));
        });

        ui.add_space(8.0);
        if section_toggle(ui, "连接诊断", self.diagnostics_open) {
            self.diagnostics_open = !self.diagnostics_open;
        }
        if self.diagnostics_open {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let task_id = self
                    .task_id
                    .as_deref()
                    .map(|value| value.chars().take(8).collect::<String>())
                    .unwrap_or_else(|| "—".to_owned());
                ui.small(format!("当前数据：{}", self.source_label));
                ui.small(format!("任务编号：{task_id}"));
                ui.small(format!("接口协议：{}", self.schema_version));
                if let Some(error) = &self.upstream_error {
                    ui.small(format!("机器人连接详情：{error}"));
                }
                ui.small(self.client.endpoint().display_url());
                ui.small(rerun_url);
                ui.horizontal(|ui| {
                    if ui.small_button("重新检查连接").clicked() {
                        self.refresh(ui.ctx().clone());
                    }
                    if ui.small_button("重新加载实时画面").clicked() {
                        self.selected_replay = None;
                        self.source_label = "实时数据".to_owned();
                        reconnect = true;
                    }
                });
            });
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.small("Woosh Viewer 0.1.0");
        });
        reconnect
    }
}

fn status_dot(ui: &mut egui::Ui, online: bool) {
    let color = if online {
        egui::Color32::from_rgb(70, 210, 145)
    } else {
        egui::Color32::from_rgb(120, 128, 142)
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn task_status_label(status: &str) -> &'static str {
    match status.to_ascii_lowercase().as_str() {
        "processing" | "busy" => "执行中",
        "completed" | "success" | "succeeded" => "已完成",
        "failed" | "error" | "aborted" => "异常",
        "cancelled" | "canceled" | "stopped" => "已停止",
        _ => "待命",
    }
}

fn section_toggle(ui: &mut egui::Ui, title: &str, open: bool) -> bool {
    ui.add(
        egui::Button::new(format!("{} {title}", if open { "▾" } else { "▸" }))
            .min_size(egui::vec2(ui.available_width(), 30.0)),
    )
    .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_connection_builds_local_sidecar_endpoints() {
        let connection = SidecarSettings::from_input("robot", 8008, 8010, 9876)
            .unwrap()
            .connection();

        assert_eq!(
            connection.control_endpoint(),
            ControlEndpoint {
                host: "127.0.0.1".to_owned(),
                port: 8010,
            }
        );
        assert_eq!(connection.rerun_url(), "rerun+http://127.0.0.1:9876/proxy");
    }

    #[test]
    fn settings_reject_urls_and_zero_ports() {
        assert!(SidecarSettings::from_input("http://robot", 8008, 8010, 9876).is_err());
        assert!(SidecarSettings::from_input("robot", 0, 8010, 9876).is_err());
        assert!(SidecarSettings::from_input("robot", 8008, 0, 9876).is_err());
        assert!(SidecarSettings::from_input("robot", 8008, 8010, 0).is_err());
    }

    #[test]
    fn saved_connection_round_trips_as_viewer_config() {
        let settings = SidecarSettings::from_input("192.168.4.38", 8008, 8010, 9876).unwrap();
        let contents = connection_config_contents(&settings).unwrap();
        let config: FileConfig = toml::from_str(&contents).unwrap();

        assert_eq!(config.robot_ip.as_deref(), Some("192.168.4.38"));
        assert_eq!(config.robot_port, Some(8008));
        assert_eq!(config.control_port, Some(8010));
        assert_eq!(config.rerun_port, Some(9876));
        assert!(config.rerun_url.is_none());
    }
}
