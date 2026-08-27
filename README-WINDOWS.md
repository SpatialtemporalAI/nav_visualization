# Woosh Viewer for Windows

这是可直接运行的 Windows 机器人操作端。Viewer、机器人数据接入、Rerun 服务、
动态地图转换和本机任务录制已经全部集成在同一个 Rust 可执行文件中。

发行包不包含 Python、PyArrow、uv、第二套 Rerun 原生绑定或 ROS 2。目标电脑无需
联网准备依赖，也无需安装开发环境。

## 直接使用

1. 解压 `woosh-viewer-windows-x64.zip`；
2. 确保电脑与机器人处于同一网络，并能访问 `http://<机器人IP>:8008`；
3. 双击 `woosh-viewer.exe`；
4. 在左侧填写机器人 IP，点击“连接机器人”。

按 Enter 不会提交导航任务。填写目标地点并确认选项后，必须点击“开始导航”。

连接设置保存在 exe 旁的 `woosh-viewer.toml`。本机任务回放保存在：

```text
%LOCALAPPDATA%\Woosh\rerun-history
```

## 发行包内容

```text
woosh-viewer.exe      Viewer 与内置数据服务
woosh-viewer.toml     机器人地址和本机 Rerun 端口
```

首次启动时机器人 IP 默认为空，应用会自动打开“连接设置”。输入机器人 IP 后点击
“连接机器人”，勾选保存后下次启动会直接使用该地址。程序没有首次启动下载或安装步骤。

## 从源码构建

构建电脑需要 Visual Studio 2022 Build Tools（使用 C++ 的桌面开发）和 Rust
1.95.0。在 PowerShell 中执行：

```powershell
cd C:\path\to\woosh-windows\operator\woosh_viewer
.\build-windows.ps1
```

构建缓存默认保留，以便后续快速增量编译。仅在需要释放磁盘空间时增加
`-CleanBuildCache`。

产物位于：

```text
operator\woosh_viewer\dist\windows-x64
operator\woosh_viewer\dist\woosh-viewer-windows-x64.zip
operator\woosh_viewer\dist\woosh-viewer-windows-x64.zip.sha256
```

## 运行边界

- Windows Viewer 只在观察数据时连接 `/viz/ws`、读取地图和图像；
- 只有点击明确的导航或停止按钮才会向机器人发送控制请求；
- 机器人继续运行现有的 `robot_nav:8008`，无需部署这个 Windows 包中的文件。
