# Woosh Windows 轻量包

这个包只包含 Windows operator 和远程 Rerun sidecar 所需的源码与锁文件。
它不包含 Git 历史、机器人导航节点、地图、测试、日志或缓存，也不会在机器人上
安装或启动任何服务。

## 包内结构

```text
operator/woosh_viewer/       Windows 原生可视化界面及构建脚本
rerun_bridge/                锁定的 Python/Rerun 环境
src/run_rerun_sidecar.py     Windows 端 sidecar 入口
src/visualization/           sidecar 必需的数据转换代码
docs/rerun_sidecar.md        架构与端口说明
```

## Windows 首次准备

Windows 电脑需要和机器人处于同一网络，并能访问：

```text
http://<机器人IP>:8008
```

安装 `uv`。官方 Windows 安装说明：
<https://docs.astral.sh/uv/getting-started/installation/>

如果包里还没有预编译的 `woosh-viewer.exe`，构建电脑还需要：

1. Visual Studio 2022 Build Tools，并选择“使用 C++ 的桌面开发”；
2. 通过 <https://rustup.rs/> 安装 Rust；
3. 本项目的 `rust-toolchain.toml` 会选择 Rust 1.95.0。

在 PowerShell 中构建：

```powershell
cd C:\path\to\woosh-windows\operator\woosh_viewer
.\build-windows.ps1
```

生成的程序位于：

```text
operator\woosh_viewer\dist\windows-x64\woosh-viewer.exe
```

构建只需进行一次。日常使用 operator 不需要 Rust 或 Visual Studio。

## 每次连接机器人

直接双击：

```text
dist\windows-x64\woosh-viewer.exe
```

在左侧“机器人连接”中填写：

```text
机器人 IP    192.168.123.161
机器人端口  8008
控制端口    8010
Rerun 端口  9876
```

点击“启动 sidecar 并连接”。Viewer 会在后台自动启动本机 sidecar，首次启动会
安装锁定的 Python、Rerun 和网络依赖，可能需要等待几分钟。后台输出保存在 exe
旁的 `woosh-sidecar.log`。设置会保存到 `woosh-viewer.toml`，以后只需双击 Viewer。

## 运行边界

- Windows：运行 sidecar、数据转换、Rerun、回放保存和 operator；
- 机器人：只保持现有的 `robot_nav:8008`；
- 不要在机器人上启动这个包中的任何程序；
- sidecar 启动和正常观察不会发送导航命令，只有界面中的明确操作才会转发控制请求。
