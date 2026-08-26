# Woosh Viewer for Windows

Woosh Viewer 是一个面向 Windows 的机器人导航观察与控制端。它连接机器人上已经运行的
WebViz 服务，在 Windows 电脑上完成 Rerun 可视化、任务回放保存和操作员界面，不会在
机器人上启动第二套导航或 ROS 节点。

## 工作方式

```text
机器人 robot_nav:8008
  ├─ /viz/ws                  状态流（只读）
  ├─ /viz/api/frame/...       相机图像（只读）
  ├─ /viz/api/map/...         地图（只读）
  └─ navigation POST routes   仅响应操作员的明确操作
                 │
                 ▼
Windows sidecar
  ├─ 127.0.0.1:8010           控制兼容 API
  ├─ 127.0.0.1:9876           Rerun 数据流
  └─ log/rerun_sidecar_history/*.rrd
                 │
                 ▼
Woosh Viewer 原生桌面界面
```

正常观察只读取机器人数据。只有在界面中提交导航、停止导航或切换动态地图录制时，
sidecar 才会向机器人发送对应的 POST 请求。

## 使用

1. 下载或克隆本仓库。
2. 安装 [uv](https://docs.astral.sh/uv/getting-started/installation/)。
3. 按下方说明构建 Viewer，或使用已有的 Windows 构建产物。
4. 双击 `woosh-viewer.exe`，填写机器人地址并选择“启动 sidecar 并连接”。

首次连接时，uv 会根据锁文件准备 Python 和 Rerun 环境，可能需要几分钟。后续启动会复用
本地环境。具体说明见 [Windows 使用指南](README-WINDOWS.md)。

## 从源码构建

构建环境需要：

- Windows 10/11 x64；
- Visual Studio 2022 Build Tools，并安装“使用 C++ 的桌面开发”；
- Rust 1.95.0（仓库中的 toolchain 文件会选择该版本）；
- PowerShell 5.1 或更高版本；
- uv（运行 sidecar 和执行 Python 检查时需要）。

```powershell
cd operator\woosh_viewer
.\build-windows.ps1 -KeepBuildCache
```

生成的可运行目录位于 `operator\woosh_viewer\dist\windows-x64`。

## 开发检查

```powershell
uv sync --project .\rerun_bridge --extra sidecar --locked
uv run --project .\rerun_bridge --extra sidecar --locked python -m compileall -q src

cd operator\woosh_viewer
cargo fmt --all -- --check
cargo test --locked
```

## 项目结构

```text
src/visualization/             Python 数据转换、Rerun 和回放逻辑
src/run_rerun_sidecar.py       Windows sidecar 入口
operator/woosh_viewer/         Rust 原生 Viewer 与发布脚本
rerun_bridge/                  锁定的 Python/Rerun 环境
docs/                          架构文档
```

## 版本与许可证

Rerun SDK 和原生 Viewer 固定为 `0.36.1`，升级时应同时验证 Python 数据端和 Rust
Viewer。项目按 MIT 或 Apache-2.0 双许可证发布，任选其一使用。
