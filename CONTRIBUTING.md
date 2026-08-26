# 贡献指南

## 开发环境

Python 环境使用 uv 和 `rerun_bridge/uv.lock`，Rust 环境使用
`operator/woosh_viewer/rust-toolchain.toml`。提交代码时不要提交虚拟环境、`target`、
`dist`、运行日志、机器人地图或回放文件。

## 提交前检查

在仓库根目录运行：

```powershell
uv sync --project .\rerun_bridge --extra sidecar --locked
uv run --project .\rerun_bridge --extra sidecar --locked python -m compileall -q src
```

然后运行：

```powershell
cd operator\woosh_viewer
cargo fmt --all -- --check
cargo test --locked
```

涉及连接或端口的改动，还应在隔离网络中通过模拟服务或测试机器人执行一次手动验证。

## 安全边界

- 不要让 sidecar 默认监听局域网地址；控制 API 和 Rerun 服务默认仅绑定回环地址。
- 不要在机器人已有导航进程旁启动第二套导航或 ROS 节点。
- 导航控制必须来自操作员明确操作，连接和观察流程不得隐式发送控制请求。
- Issue、日志和回放中不得包含密钥、内部网络信息或敏感地图数据。
