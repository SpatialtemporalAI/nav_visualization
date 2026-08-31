# Changelog

本项目的版本记录遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- 新增 macOS Universal Application 构建，单一安装包同时支持 Apple Silicon 与 Intel Mac。
- 新增 GitHub Actions macOS 双架构构建、合并、签名及 ZIP/DMG 打包流程。
- macOS 配置与任务记录写入 `~/Library/Application Support/Woosh`，不会修改应用包。

### Fixed

- 首次填写或运行中修改机器人 IP 后，强制重新挂载本机 Rerun 实时源；不再需要重启应用才能显示地图、Robot/VPR 定位和动态画面。
- 将占用栅格底图记录为静态 PNG，切换 `navigation_time` 时不再只显示动态地图层。
- Windows Release 改用 GUI 子系统，双击启动不再弹出终端窗口。
- 修改机器人 IP 或点击重新加载后，内置服务会立即断开旧数据流并连接新地址，无需重启应用。
- 识别机器人实际返回的 `processing` 和 `busy` 导航状态，停止按钮不再因状态误判而消失。
- 连接新机器人时不再写入永久静态清空，Robot、VPR 和动态地图可随新数据立即恢复显示。
- 实时 Viewer 时间轴使用消息到达时间，切换到系统时钟不同的机器人时新帧不会落到旧时间点之后。

### Changed

- Windows 发布包仅保留 Viewer 可执行文件和配置文件，默认机器人 IP 留空并在首次启动时提示输入。
- 左侧栏取消整体滚动，把连接设置、任务记录和连接诊断收纳为独立弹窗。
- 实时位置、规划模式、地图版本和路径点数集成到右侧 Rerun 视图，左侧只保留控制操作。
- Robot、VPR 与目标图标恢复固定屏幕尺寸的原有圆形样式，各图层悬停时显示同一份完整定位信息。
- 缩小定位图标和朝向线；Robot 比 VPR 更大且绘制在前方，地图缩小时保持清晰的主次关系。
- Robot 与目标朝向改为从圆心出发的箭头，方向与实时 `theta` 一致。
- 右侧改为上方地图/相机，下方 Event 与实时数据并排；实时指标使用原生 Rerun 文档视图。
- 实时数据使用普通 Markdown 文本，移除数值周围不必要的代码块背景。
- Windows 构建使用 Cargo 可用的全部核心，加快内嵌 Rerun Viewer 编译。

## [0.2.1] - 2026-08-31

### Fixed

- 停止导航或任务自然结束后，“停止导航”按钮立即恢复为灰色禁用状态，不再与运行中的导航混淆。
- 停止接口成功后同步更新内置实时状态，避免旧 WebSocket 快照再次点亮停止按钮。
- macOS 打包时从 `Cargo.toml` 自动写入应用版本，避免安装包版本信息滞后。

## [0.2.0] - 2026-08-26

### Changed

- 将 Python Sidecar 的数据接入、Rerun 转换和任务录制迁入 Rust Viewer。
- Windows Release 移除 Python、PyArrow、FastAPI 和第二套 Rerun 原生绑定。
- Robot、VPR 与目标定位改为圆形标记，文字仅在悬停时显示。

## [0.1.0] - 2026-08-26

### Added

- Windows 原生 Woosh Viewer。
- 连接现有 WebViz 服务的远程 Rerun sidecar。
- 导航、停止和动态地图录制控制。
- 本地 Rerun 任务录制与历史回放。
- 可复现的 uv 和 Cargo 锁定依赖。
