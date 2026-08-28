# Woosh Viewer

面向 Woosh 机器人导航的原生操作端，支持 Windows 与 macOS。应用内嵌 Rerun
Viewer，并由同一个 Rust 进程完成机器人数据接入、地图与相机加载、动态障碍转换和
本机任务回放。

## 下载

在 [v0.2.0 Release](https://github.com/SpatialtemporalAI/nav_visualization/releases/tag/v0.2.0)
中按电脑平台下载：

| 平台 | 推荐安装包 |
| --- | --- |
| Windows x64 | `woosh-viewer-windows-x64.zip` |
| macOS Apple Silicon | `woosh-viewer-macos-arm64.dmg` |
| macOS Intel | `woosh-viewer-macos-intel-x64.dmg` |

Release 已包含 Viewer 和原生数据服务。用户无需安装 Python、Rust、ROS 2 或独立
Sidecar，也没有首次启动联网下载依赖的步骤。

## 特性

- Robot、VPR 与目标定位采用有辨识度的圆形标记，信息仅在悬停时显示；
- 地图、全局/局部路径、动态障碍、前置相机和 NavDP 画面集中展示；
- 导航必须点击“开始导航”，Enter 不会误提交；
- 可在应用内切换机器人地址，并自动重连数据流；
- 任务 `.rrd` 自动保存到本机，直接使用 Rerun 时间轴回放；
- Release 无 Python、PyArrow、uv、FastAPI 和额外 Rerun 可执行文件。

## 数据流

```text
robot_nav :8008
  ├─ /viz/ws ───────────────┐
  ├─ map / camera images ───┤
  └─ explicit controls ◀────┤
                            │
Woosh Viewer (Rust)
  ├─ native data adapter
  ├─ embedded Rerun server + Viewer
  └─ local rerun-history/*.rrd
```

正常观察只读取数据；只有点击导航、停止或录制开关时才会发送相应控制请求。

## 使用 Release

从 [GitHub Release](https://github.com/SpatialtemporalAI/nav_visualization/releases/tag/v0.2.0)
下载并解压 `woosh-viewer-windows-x64.zip`，双击
`woosh-viewer.exe`，填写机器人 IP 后点击“连接机器人”。目标电脑不需要联网安装
依赖，也不需要 Python、Rust 或 ROS 2。

任务记录保存在 exe 同目录的 `rerun-history` 文件夹，便于随程序统一管理和删除。
完整说明见 [Windows 使用说明](README-WINDOWS.md)。

## 使用 macOS 版

macOS 版按架构分别发布。Apple Silicon Mac 使用
`woosh-viewer-macos-arm64.dmg`，Intel Mac 使用
`woosh-viewer-macos-intel-x64.dmg`。打开对应安装包后，将 **Woosh Viewer**
拖入 Applications 即可。
应用无需 Python 或独立 Sidecar，首次连接时按系统提示允许访问本地网络。

完整说明见 [macOS 使用说明](operator/woosh_viewer/README-MACOS.md)。

## 文档

- [Windows 安装、连接与构建](README-WINDOWS.md)
- [macOS 安装、签名与构建](operator/woosh_viewer/README-MACOS.md)
- [Viewer 源码说明与接口](operator/woosh_viewer/README.md)
- [旧版 Python Sidecar（仅兼容与迁移参考）](docs/rerun_sidecar.md)
- [版本记录](CHANGELOG.md)
- [贡献指南](CONTRIBUTING.md)

## 从源码构建

构建电脑需要 Rust 1.95.0 和 Visual Studio 2022 Build Tools：

```powershell
cd operator\woosh_viewer
.\build-windows.ps1
```

构建缓存默认保留，以便后续增量编译；仅在需要释放磁盘空间时使用
`.\build-windows.ps1 -CleanBuildCache`。

产物：

```text
operator\woosh_viewer\dist\windows-x64
operator\woosh_viewer\dist\woosh-viewer-windows-x64.zip
```

仅生成 Windows Viewer 源码传输包：

```powershell
python operator\package_windows_bundle.py
```

macOS 构建需要 Xcode Command Line Tools 和 Rust 1.95.0：

```bash
cd operator/woosh_viewer
chmod +x build-macos.sh
./build-macos.sh
```

脚本会分别生成 Apple Silicon 与 Intel 的 ZIP、DMG 和 SHA-256 校验文件。GitHub
Actions 也可通过 `Build macOS` 工作流完成双架构构建。

## 开发说明

Rerun SDK 与原生 Viewer 固定为 `0.36.1`。仓库中的 Python Sidecar 文件仅作为旧版
兼容和迁移参考，不会进入当前 Windows Release。

项目采用 MIT OR Apache-2.0 双许可证。
