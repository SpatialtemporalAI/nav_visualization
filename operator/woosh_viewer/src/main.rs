#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod control_client;
mod native_sidecar;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use control_client::{ActionKind, ControlClient, ControlCommand, ControlEndpoint};
use native_sidecar::{
    NativeReplayTask as ReplayTask, NativeSidecar, NativeSidecarSettings, NativeStatusSnapshot,
    delete_replay_task, replay_tasks_in,
};
use rerun::external::{
    eframe, egui, re_crash_handler, re_log, re_log_types, re_memory, re_viewer, tokio,
};
use serde::{Deserialize, Serialize};

#[global_allocator]
static GLOBAL: re_memory::AccountingAllocator<mimalloc::MiMalloc> =
    re_memory::AccountingAllocator::new(mimalloc::MiMalloc);

#[derive(Clone, Debug)]
struct ViewerConfig {
    robot_ip: String,
    robot_port: u16,
    rerun_port: u16,
    rerun_url: Option<String>,
    screenshot: Option<PathBuf>,
    config_path: PathBuf,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            robot_ip: String::new(),
            robot_port: 8008,
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
                        "woosh-viewer [--config FILE] [--robot-ip IP] [--robot-port PORT] \
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
    rerun_port: Option<u16>,
    rerun_url: Option<String>,
}

fn default_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let user_config = default_config_write_path();
        if user_config.is_file() {
            return Some(user_config);
        }
    }
    let beside_executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("woosh-viewer.toml")));
    beside_executable.filter(|path| path.is_file()).or_else(|| {
        let path = PathBuf::from("woosh-viewer.toml");
        path.is_file().then_some(path)
    })
}

fn default_config_write_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return platform_data_dir().join("woosh-viewer.toml");
    }
    #[cfg(not(target_os = "macos"))]
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("woosh-viewer.toml")))
        .unwrap_or_else(|| PathBuf::from("woosh-viewer.toml"))
}

fn default_history_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        return std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("rerun-history")))
            .unwrap_or_else(|| PathBuf::from("rerun-history"));
    }
    #[cfg(not(target_os = "windows"))]
    platform_data_dir().join("rerun-history")
}

#[cfg(not(target_os = "windows"))]
fn platform_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Library")
            .join("Application Support")
            .join("Woosh");
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local").join("share"))
            })
            .unwrap_or_else(std::env::temp_dir)
            .join("Woosh")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveConnection {
    robot_ip: String,
    robot_port: u16,
    rerun_port: u16,
}

impl LiveConnection {
    fn from_config(config: &ViewerConfig) -> Self {
        Self {
            robot_ip: config.robot_ip.clone(),
            robot_port: config.robot_port,
            rerun_port: config.rerun_port,
        }
    }

    fn control_endpoint(&self) -> ControlEndpoint {
        ControlEndpoint {
            host: self.robot_ip.clone(),
            port: self.robot_port,
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
    rerun_port: u16,
}

impl SidecarSettings {
    fn from_config(config: &ViewerConfig) -> Result<Self, String> {
        Self::from_input(&config.robot_ip, config.robot_port, config.rerun_port)
    }

    fn from_input(robot_ip: &str, robot_port: u16, rerun_port: u16) -> Result<Self, String> {
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
        if robot_port == 0 || rerun_port == 0 {
            return Err("端口必须在 1–65535 之间".to_owned());
        }
        Ok(Self {
            robot_ip: robot_ip.to_owned(),
            robot_port,
            rerun_port,
        })
    }

    fn connection(&self) -> LiveConnection {
        LiveConnection {
            robot_ip: self.robot_ip.clone(),
            robot_port: self.robot_port,
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
    worker: Option<NativeSidecar>,
    history_dir: PathBuf,
}

impl SidecarManager {
    fn new(_log_path: PathBuf) -> Self {
        Self {
            worker: None,
            history_dir: default_history_dir(),
        }
    }

    fn restart(&mut self, settings: &SidecarSettings) -> Result<(), String> {
        let native_settings = NativeSidecarSettings {
            robot_ip: settings.robot_ip.clone(),
            robot_port: settings.robot_port,
            rerun_port: settings.rerun_port,
            history_dir: self.history_dir.clone(),
        };
        if let Some(worker) = self.worker.as_ref()
            && worker.is_running()
            && worker.rerun_port() == Some(settings.rerun_port)
        {
            return worker.reconfigure(native_settings);
        }
        self.stop();
        let worker = NativeSidecar::start(native_settings)?;
        self.worker = Some(worker);
        Ok(())
    }

    fn poll_exit(&mut self) -> Option<String> {
        let worker = self.worker.as_mut()?;
        if let Some(error) = worker.poll_error() {
            self.worker = None;
            return Some(error);
        }
        (!worker.is_running()).then(|| "内置数据服务已退出".to_owned())
    }

    fn is_running(&self) -> bool {
        self.worker.as_ref().is_some_and(NativeSidecar::is_running)
    }

    fn is_connected(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(NativeSidecar::is_connected)
    }

    fn last_message_at(&self) -> Option<f64> {
        self.worker
            .as_ref()
            .and_then(NativeSidecar::last_message_at)
    }

    fn status(&self) -> NativeStatusSnapshot {
        self.worker
            .as_ref()
            .map(NativeSidecar::status)
            .unwrap_or_default()
    }

    fn set_navigation_running_from_control(&self, running: bool) {
        if let Some(worker) = self.worker.as_ref() {
            worker.set_navigation_running_from_control(running);
        }
    }

    fn replay_tasks(&self) -> Vec<ReplayTask> {
        replay_tasks_in(&self.history_dir)
    }

    fn delete_replay_task(&self, task_dir: &std::path::Path) -> Result<(), String> {
        delete_replay_task(&self.history_dir, task_dir)
    }

    fn history_dir(&self) -> &std::path::Path {
        &self.history_dir
    }

    fn stop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.stop();
        }
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.stop();
    }
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
    let sidecar_settings = if config.robot_ip.trim().is_empty() {
        None
    } else {
        Some(SidecarSettings::from_config(&config)?)
    };
    let has_initial_connection = sidecar_settings.is_some();
    let control_endpoint = live_connection.control_endpoint();
    let rerun_port = config.rerun_port;
    let config_path = config.config_path.clone();

    eframe::run_native(
        "Woosh Robot Operator",
        native_options,
        Box::new(move |cc| {
            re_viewer::customize_eframe_and_setup_renderer(cc)?;
            let mut rerun_app = re_viewer::App::new(
                main_thread_token,
                re_viewer::build_info(),
                re_viewer::AppEnvironment::Custom("Woosh Robot Operator".to_owned()),
                startup_options,
                cc,
                None,
                re_viewer::AsyncRuntimeHandle::from_current_tokio_runtime_or_wasmbindgen()?,
            );
            // Rerun defaults to UTC. Display timestamps in the operator computer's
            // local timezone while preserving the original Unix timestamps.
            rerun_app.app_options_mut().timestamp_format =
                re_log_types::TimestampFormat::local_timezone();
            if has_initial_connection {
                rerun_app.open_url_or_file(&rerun_url);
            }

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
        let panel_fill = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(8, 8, 8)
        } else {
            egui::Color32::WHITE
        };
        let panel_border = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(52, 52, 52)
        } else {
            egui::Color32::from_rgb(214, 214, 214)
        };
        egui::Panel::left("woosh_control_panel")
            .default_size(360.0)
            .min_size(330.0)
            .max_size(450.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(panel_fill)
                    .stroke(egui::Stroke::new(1.0, panel_border))
                    .inner_margin(egui::Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| {
                apply_control_style(ui);
                self.controls.ui(ui, telemetry_loaded, &self.rerun_url);
            });
        if let Some(request) = self.controls.take_requested_connection() {
            let save_result = request
                .save
                .then(|| save_connection_config(self.controls.config_path(), &request.settings));
            let next_rerun_url = request.connection.rerun_url();
            self.rerun_url = next_rerun_url;
            self.controls.apply_connection(
                request.settings,
                request.connection,
                save_result,
                ui.ctx().clone(),
            );
            // The local Rerun URL normally stays the same when the robot changes.
            // Re-open it unconditionally so the Viewer drops any failed or stale
            // connection left by the previous robot.
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
        PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        PathBuf::from("/System/Library/Fonts/STHeiti Medium.ttc"),
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

fn apply_control_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    let dark = style.visuals.dark_mode;
    style.animation_time = 0.16;
    style.spacing.item_spacing = egui::vec2(7.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = 29.0;

    let visuals = &mut style.visuals;
    let (surface, raised, input, border, text, weak, accent) = if dark {
        (
            egui::Color32::from_rgb(19, 19, 19),
            egui::Color32::from_rgb(30, 30, 30),
            egui::Color32::from_rgb(13, 13, 13),
            egui::Color32::from_rgb(52, 52, 52),
            egui::Color32::from_rgb(238, 238, 238),
            egui::Color32::from_rgb(158, 158, 158),
            egui::Color32::from_rgb(42, 203, 225),
        )
    } else {
        (
            egui::Color32::WHITE,
            egui::Color32::from_rgb(244, 244, 244),
            egui::Color32::from_rgb(250, 250, 250),
            egui::Color32::from_rgb(214, 214, 214),
            egui::Color32::from_rgb(28, 28, 28),
            egui::Color32::from_rgb(105, 105, 105),
            egui::Color32::from_rgb(13, 143, 170),
        )
    };

    visuals.faint_bg_color = raised;
    visuals.extreme_bg_color = input;
    visuals.text_edit_bg_color = Some(input);
    visuals.override_text_color = Some(text);
    visuals.weak_text_color = Some(weak);
    visuals.selection.bg_fill = accent.gamma_multiply(0.28);
    visuals.selection.stroke = egui::Stroke::new(1.5, accent);
    visuals.warn_fg_color = egui::Color32::from_rgb(245, 183, 77);
    visuals.error_fg_color = egui::Color32::from_rgb(244, 103, 112);
    visuals.striped = false;

    visuals.widgets.noninteractive.bg_fill = surface;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(10);

    visuals.widgets.inactive.bg_fill = raised;
    visuals.widgets.inactive.weak_bg_fill = raised;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(9);

    visuals.widgets.hovered.bg_fill = accent.gamma_multiply(if dark { 0.18 } else { 0.12 });
    visuals.widgets.hovered.weak_bg_fill = accent.gamma_multiply(if dark { 0.18 } else { 0.12 });
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, text);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(9);

    visuals.widgets.active.bg_fill = accent.gamma_multiply(if dark { 0.28 } else { 0.18 });
    visuals.widgets.active.weak_bg_fill = accent.gamma_multiply(if dark { 0.28 } else { 0.18 });
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, text);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(9);
}

fn accent_color(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(42, 203, 225)
    } else {
        egui::Color32::from_rgb(13, 143, 170)
    }
}

fn centered_button(text: egui::RichText) -> egui::Button<'static> {
    egui::Button::new((egui::Atom::grow(), text, egui::Atom::grow()))
}

fn card_frame(ui: &egui::Ui) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    let fill = if dark {
        egui::Color32::from_rgb(19, 19, 19)
    } else {
        egui::Color32::WHITE
    };
    let border = if dark {
        egui::Color32::from_rgb(52, 52, 52)
    } else {
        egui::Color32::from_rgb(214, 214, 214)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(12, 10))
}

fn brand_header(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("WOOSH").size(18.0).strong());
        ui.label(egui::RichText::new("ROBOT OPERATOR").size(10.0).weak());
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("外观").size(11.0).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::global_theme_preference_buttons(ui);
        });
    });
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
    connection_rerun_port: u16,
    connection_save: bool,
    connection_notice: Option<(String, bool)>,
    connection_configured: bool,
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
    replay_delete_confirmation: Option<ReplayTask>,
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
        settings: Option<SidecarSettings>,
    ) -> Self {
        let log_path = config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("woosh-sidecar.log");
        let mut sidecar = SidecarManager::new(log_path);
        let start_result = settings.as_ref().map(|settings| sidecar.restart(settings));
        let connection_configured = settings.is_some();
        let connection_robot_ip = settings
            .as_ref()
            .map(|settings| settings.robot_ip.clone())
            .unwrap_or_else(|| endpoint.host.clone());
        let connection_robot_port = settings
            .as_ref()
            .map_or(endpoint.port, |settings| settings.robot_port);
        let mut panel = Self {
            client: ControlClient::new(endpoint),
            sidecar,
            config_path,
            connection_open: !connection_configured,
            advanced_connection_open: false,
            diagnostics_open: false,
            connection_robot_ip,
            connection_robot_port,
            connection_rerun_port: rerun_port,
            connection_save: true,
            connection_notice: None,
            connection_configured,
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
            replay_delete_confirmation: None,
            selected_replay: None,
            source_label: "实时数据".to_owned(),
            requested_source: None,
            pending: HashSet::new(),
            message: if connection_configured {
                "正在启动内置数据服务并连接机器人…".to_owned()
            } else {
                "首次使用，请在连接设置中输入机器人 IP".to_owned()
            },
            message_is_error: false,
        };
        if let Some(Err(err)) = start_result {
            panel.message = err.clone();
            panel.message_is_error = true;
            panel.connection_notice = Some((err, true));
            panel.connection_open = true;
        }
        panel
    }

    fn refresh(&mut self, ctx: egui::Context) {
        if !self.connection_configured {
            self.connection_open = true;
            self.message = "请先输入机器人 IP 并点击“连接机器人”".to_owned();
            self.message_is_error = false;
            return;
        }
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
        if !self.connection_configured {
            "等待连接设置"
        } else if !self.sidecar.is_running() {
            "正在启动内置服务"
        } else if !self.control_online {
            "正在连接控制服务"
        } else if !self.upstream_connected {
            "正在连接机器人"
        } else if !self.data_is_live() {
            "正在等待实时数据"
        } else {
            "已就绪"
        }
    }

    fn connection_guidance(&self) -> String {
        if !self.connection_configured {
            return "请打开“连接设置”，输入机器人 IP 后点击“连接机器人”".to_owned();
        }
        if !self.sidecar.is_running() {
            return "内置数据服务未运行，请展开“机器人连接”后重新连接".to_owned();
        }
        if !self.control_online {
            return format!(
                "无法连接机器人控制服务 {}:{}，请检查 IP、网络和机器人服务",
                self.connection_robot_ip, self.connection_robot_port
            );
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
        if !self.connection_configured
            || self.pending.contains(&ActionKind::Status)
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
        self.connection_configured = true;
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
                    format!("已连接，配置已保存到 {}", self.config_path.display()),
                    false,
                )),
                Some(Err(err)) => Some((format!("已连接，但{err}"), true)),
                None => Some(("已连接（配置未保存）".to_owned(), false)),
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
        self.upstream_connected = self.sidecar.is_connected();
        self.last_upstream_message_at = self.sidecar.last_message_at();
        let native_status = self.sidecar.status();
        self.schema_version = native_status.schema_version;
        self.task_id = native_status.task_id;
        self.task_status = native_status.task_status;
        self.task_goal = native_status.goal_text;
        self.navigation_running = native_status.navigation_running;
        self.recording_enabled = native_status.recording_enabled;
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
                    if let Some(running) = data.navigation_running {
                        self.navigation_running = running;
                        self.sidecar.set_navigation_running_from_control(running);
                        if running {
                            if !is_active_task_status(&self.task_status) {
                                self.task_status = "accepted".to_owned();
                            }
                        } else if is_active_task_status(&self.task_status) {
                            self.task_status = "stopped".to_owned();
                        }
                    }
                }
                Err(err) => match event.kind {
                    ActionKind::Status => self.control_online = false,
                    ActionKind::Refresh => {
                        self.control_online = false;
                        self.message = err;
                        self.message_is_error = true;
                    }
                    _ => {
                        self.message = err;
                        self.message_is_error = true;
                    }
                },
            }
        }
    }

    fn connection_ui(&mut self, ui: &mut egui::Ui) {
        card_frame(ui).show(ui, |ui| {
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
            ui.small("Viewer 内置数据服务，连接后即可直接使用。");
            ui.checkbox(&mut self.connection_save, "保存到 woosh-viewer.toml");
            if ui
                .add(
                    centered_button(egui::RichText::new("连接机器人").strong())
                        .fill(accent_color(ui.visuals().dark_mode))
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(9)
                        .min_size(egui::vec2(ui.available_width(), 38.0)),
                )
                .clicked()
            {
                match SidecarSettings::from_input(
                    &self.connection_robot_ip,
                    self.connection_robot_port,
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
                        self.connection_notice = Some(("正在连接机器人…".to_owned(), false));
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
        let mut open_task = None;
        let mut request_delete = None;
        let mut confirm_delete = None;
        let mut cancel_delete = false;
        card_frame(ui).show(ui, |ui| {
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
                if ui.button("刷新").clicked() {
                    self.replay_tasks = self.sidecar.replay_tasks();
                    self.replay_loaded = true;
                    self.replay_error = None;
                }
            });
            ui.small(format!(
                "记录目录：{}",
                self.sidecar.history_dir().display()
            ));
            ui.small("选择记录可在右侧时间轴中播放，也可删除不再需要的记录。");
            if let Some(error) = &self.replay_error {
                ui.colored_label(egui::Color32::from_rgb(235, 95, 105), error);
            }
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for task in self
                        .replay_tasks
                        .iter()
                        .filter(|task| task.has_rerun_recording)
                        .take(30)
                    {
                        let short_id = task.task_id.chars().take(8).collect::<String>();
                        let goal = task.goal_text.as_deref().unwrap_or("未命名任务");
                        let label = format!("{goal}\n{short_id} · {}", task.status);
                        ui.horizontal(|ui| {
                            let delete_width = 52.0;
                            if ui
                                .add_sized(
                                    [ui.available_width() - delete_width - 8.0, 42.0],
                                    egui::Button::selectable(
                                        self.selected_replay.as_deref()
                                            == Some(task.task_id.as_str()),
                                        label,
                                    ),
                                )
                                .clicked()
                            {
                                open_task = Some((
                                    task.task_id.clone(),
                                    goal.to_owned(),
                                    task.recording_source.clone(),
                                ));
                            }
                            if ui
                                .add_sized([delete_width, 30.0], egui::Button::new("删除"))
                                .on_hover_text("删除该任务的 Rerun 录制与元数据")
                                .clicked()
                            {
                                request_delete = Some(task.clone());
                            }
                        });
                    }
                });
            if let Some(task) = self.replay_delete_confirmation.as_ref() {
                ui.separator();
                let goal = task.goal_text.as_deref().unwrap_or("未命名任务");
                ui.colored_label(
                    egui::Color32::from_rgb(235, 95, 105),
                    format!("确认永久删除“{goal}”？此操作无法撤销。"),
                );
                ui.horizontal(|ui| {
                    if ui.button("确认删除").clicked() {
                        confirm_delete = Some(task.clone());
                    }
                    if ui.button("取消").clicked() {
                        cancel_delete = true;
                    }
                });
            }
            if self.replay_loaded && replay_count == 0 && self.replay_error.is_none() {
                ui.weak("暂无带 Rerun 录制的历史任务");
            }
        });
        if let Some(task) = request_delete {
            self.replay_delete_confirmation = Some(task);
            self.replay_error = None;
        }
        if cancel_delete {
            self.replay_delete_confirmation = None;
        }
        if let Some(task) = confirm_delete {
            match self.sidecar.delete_replay_task(&task.task_dir) {
                Ok(()) => {
                    if self.selected_replay.as_deref() == Some(task.task_id.as_str()) {
                        self.selected_replay = None;
                        self.source_label = "实时数据".to_owned();
                        self.requested_source = Some(rerun_url.to_owned());
                    }
                    self.replay_tasks = self.sidecar.replay_tasks();
                    self.replay_delete_confirmation = None;
                    self.replay_error = None;
                }
                Err(err) => {
                    self.replay_error = Some(err);
                    self.replay_delete_confirmation = None;
                }
            }
        }
        if let Some((task_id, goal, recording_source)) = open_task {
            self.requested_source = Some(recording_source);
            self.selected_replay = Some(task_id);
            self.source_label = format!("回放：{goal}");
        }
    }

    fn tool_buttons_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let width = ((ui.available_width() - 16.0) / 3.0).max(82.0);
            if ui
                .add_sized([width, 32.0], egui::Button::new("连接设置"))
                .on_hover_text("修改机器人地址和本机 Rerun 端口")
                .clicked()
            {
                self.connection_open = true;
            }
            if ui
                .add_sized([width, 32.0], egui::Button::new("任务记录"))
                .on_hover_text("查看保存在当前电脑的任务回放")
                .clicked()
            {
                if !self.replay_loaded {
                    self.replay_tasks = self.sidecar.replay_tasks();
                    self.replay_loaded = true;
                    self.replay_error = None;
                }
                self.replay_open = true;
            }
            if ui
                .add_sized([width, 32.0], egui::Button::new("连接诊断"))
                .on_hover_text("查看数据来源、协议和连接地址")
                .clicked()
            {
                self.diagnostics_open = true;
            }
        });
    }

    fn diagnostics_ui(&mut self, ui: &mut egui::Ui, rerun_url: &str) {
        card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let task_id = self
                .task_id
                .as_deref()
                .map(|value| value.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "—".to_owned());
            egui::Grid::new("woosh_diagnostics_grid")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("当前数据");
                    ui.label(&self.source_label);
                    ui.end_row();
                    ui.label("任务编号");
                    ui.label(task_id);
                    ui.end_row();
                    ui.label("接口协议");
                    ui.label(&self.schema_version);
                    ui.end_row();
                    ui.label("机器人控制");
                    ui.label(self.client.endpoint().display_url());
                    ui.end_row();
                    ui.label("本机 Rerun");
                    ui.label(rerun_url);
                    ui.end_row();
                });
            if let Some(error) = &self.upstream_error {
                ui.add_space(6.0);
                ui.colored_label(
                    egui::Color32::from_rgb(235, 95, 105),
                    format!("连接详情：{error}"),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("重新检查连接").clicked() {
                    self.refresh(ui.ctx().clone());
                }
                if ui.button("重新加载实时画面").clicked() {
                    self.selected_replay = None;
                    self.source_label = "实时数据".to_owned();
                    match SidecarSettings::from_input(
                        &self.connection_robot_ip,
                        self.connection_robot_port,
                        self.connection_rerun_port,
                    ) {
                        Ok(settings) => {
                            let connection = settings.connection();
                            self.requested_connection = Some(ConnectionRequest {
                                settings,
                                connection,
                                save: false,
                            });
                        }
                        Err(err) => {
                            self.message = err;
                            self.message_is_error = true;
                        }
                    }
                }
            });
        });
    }

    fn dialogs_ui(&mut self, ctx: &egui::Context, rerun_url: &str) {
        let mut connection_open = self.connection_open;
        if connection_open {
            egui::Window::new("机器人连接设置")
                .id(egui::Id::new("woosh_connection_dialog"))
                .open(&mut connection_open)
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| self.connection_ui(ui));
        }
        self.connection_open = connection_open;

        let mut replay_open = self.replay_open;
        if replay_open {
            egui::Window::new("本机任务记录")
                .id(egui::Id::new("woosh_replay_dialog"))
                .open(&mut replay_open)
                .collapsible(false)
                .resizable(true)
                .default_width(500.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| self.replay_ui(ui, rerun_url));
        }
        self.replay_open = replay_open;

        let mut diagnostics_open = self.diagnostics_open;
        if diagnostics_open {
            egui::Window::new("连接诊断")
                .id(egui::Id::new("woosh_diagnostics_dialog"))
                .open(&mut diagnostics_open)
                .collapsible(false)
                .resizable(false)
                .default_width(520.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| self.diagnostics_ui(ui, rerun_url));
        }
        self.diagnostics_open = diagnostics_open;
    }

    fn ui(&mut self, ui: &mut egui::Ui, telemetry_loaded: bool, rerun_url: &str) {
        brand_header(ui);
        ui.add_space(6.0);

        self.tool_buttons_ui(ui);
        ui.add_space(6.0);

        let ready = self.navigation_ready() && telemetry_loaded;
        let stage_color = if ready {
            egui::Color32::from_rgb(70, 210, 145)
        } else if self.message_is_error {
            egui::Color32::from_rgb(235, 95, 105)
        } else {
            egui::Color32::from_rgb(245, 166, 35)
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("运行状态").size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(self.startup_label())
                        .color(stage_color)
                        .strong(),
                );
                if !ready && !self.message_is_error {
                    ui.spinner();
                }
            });
        });
        ui.add_space(4.0);
        card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                status_dot(ui, self.sidecar.is_running());
                ui.strong("内置数据服务");
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

        ui.add_space(8.0);
        ui.label(egui::RichText::new("发送导航任务").size(15.0).strong());
        ui.small("选择地点，或用自然语言描述机器人要去的位置");
        ui.add_space(4.0);
        card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
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
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(10, 8)),
                );
                ui.checkbox(&mut self.dry_run, "仅规划路线，不让机器人移动")
                    .on_hover_text("用于确认目标和路线，机器人不会执行移动");
                let mut recording = self.recording_enabled;
                if ui
                    .checkbox(&mut recording, "保存本次任务的动态地图")
                    .changed()
                {
                    self.recording_enabled = recording;
                    self.dispatch(ControlCommand::SetRecording(recording), ui.ctx().clone());
                }

                let navigate_pending = self.pending.contains(&ActionKind::Navigate);
                let can_navigate = !self.goal_text.trim().is_empty()
                    && !navigate_pending
                    && !self.navigation_running;
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
                        centered_button(egui::RichText::new(navigate_text).strong())
                            .fill(accent_color(ui.visuals().dark_mode))
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(9)
                            .min_size(egui::vec2(ui.available_width(), 36.0)),
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
            });
            ui.add_space(5.0);
            let stop_pending = self.pending.contains(&ActionKind::Stop);
            let can_stop =
                can_stop_navigation(self.control_online, self.navigation_running, stop_pending);
            let stop_active = self.navigation_running || stop_pending;
            let stop_button = centered_button(
                egui::RichText::new(if stop_pending {
                    "正在停止…"
                } else if self.navigation_running {
                    "停止导航"
                } else {
                    "当前无导航任务"
                })
                .color(if stop_active {
                    egui::Color32::WHITE
                } else {
                    ui.visuals().weak_text_color()
                })
                .strong(),
            )
            .fill(if stop_active {
                egui::Color32::from_rgb(205, 67, 73)
            } else {
                ui.visuals().faint_bg_color
            })
            .stroke(if stop_active {
                egui::Stroke::NONE
            } else {
                ui.visuals().widgets.inactive.bg_stroke
            })
            .corner_radius(9)
            .min_size(egui::vec2(ui.available_width(), 36.0));
            let disabled_reason = if !self.control_online {
                "连接机器人控制服务后可用"
            } else if !self.navigation_running {
                "当前没有正在执行的导航任务"
            } else {
                "停止请求正在处理中"
            };
            if ui
                .add_enabled(can_stop, stop_button)
                .on_disabled_hover_text(disabled_reason)
                .clicked()
            {
                self.dispatch(ControlCommand::Stop, ui.ctx().clone());
            }
        });

        ui.add_space(8.0);
        card_frame(ui).show(ui, |ui| {
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

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.small(format!("Woosh Viewer {}", env!("CARGO_PKG_VERSION")));
        });
        self.dialogs_ui(ui.ctx(), rerun_url);
    }
}

fn status_dot(ui: &mut egui::Ui, online: bool) {
    let color = if online {
        egui::Color32::from_rgb(77, 222, 155)
    } else {
        egui::Color32::from_rgb(120, 128, 142)
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 6.0, color.gamma_multiply(0.16));
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

fn task_status_label(status: &str) -> &'static str {
    if is_active_task_status(status) {
        return "执行中";
    }
    match status.to_ascii_lowercase().as_str() {
        "completed" | "success" | "succeeded" => "已完成",
        "failed" | "error" | "aborted" => "异常",
        "cancelled" | "canceled" | "stopped" => "已停止",
        _ => "待命",
    }
}

fn is_active_task_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "accepted"
            | "queued"
            | "running"
            | "planning"
            | "navigating"
            | "executing"
            | "processing"
            | "busy"
    )
}

fn can_stop_navigation(control_online: bool, navigation_running: bool, stop_pending: bool) -> bool {
    control_online && navigation_running && !stop_pending
}

fn section_toggle(ui: &mut egui::Ui, title: &str, open: bool) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(format!("{}  {title}", if open { "−" } else { "+" })).strong(),
        )
        .frame(false)
        .corner_radius(8)
        .min_size(egui::vec2(ui.available_width(), 32.0)),
    )
    .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_connection_uses_robot_control_and_local_rerun() {
        let connection = SidecarSettings::from_input("robot", 8008, 9876)
            .unwrap()
            .connection();

        assert_eq!(
            connection.control_endpoint(),
            ControlEndpoint {
                host: "robot".to_owned(),
                port: 8008,
            }
        );
        assert_eq!(connection.rerun_url(), "rerun+http://127.0.0.1:9876/proxy");
    }

    #[test]
    fn settings_reject_urls_and_zero_ports() {
        assert!(SidecarSettings::from_input("", 8008, 9876).is_err());
        assert!(SidecarSettings::from_input("http://robot", 8008, 9876).is_err());
        assert!(SidecarSettings::from_input("robot", 0, 9876).is_err());
        assert!(SidecarSettings::from_input("robot", 8008, 0).is_err());
    }

    #[test]
    fn saved_connection_round_trips_as_viewer_config() {
        let settings = SidecarSettings::from_input("192.168.4.38", 8008, 9876).unwrap();
        let contents = connection_config_contents(&settings).unwrap();
        let config: FileConfig = toml::from_str(&contents).unwrap();

        assert_eq!(config.robot_ip.as_deref(), Some("192.168.4.38"));
        assert_eq!(config.robot_port, Some(8008));
        assert_eq!(config.rerun_port, Some(9876));
        assert!(config.rerun_url.is_none());
    }

    #[test]
    fn stop_button_requires_an_active_navigation() {
        assert!(!can_stop_navigation(true, false, false));
        assert!(!can_stop_navigation(false, true, false));
        assert!(!can_stop_navigation(true, true, true));
        assert!(can_stop_navigation(true, true, false));
    }
}
