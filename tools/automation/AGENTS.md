# automation（`lectern-automation`）

自动化构建：与 `lectern` 相同的应用，在每个窗口里装 [gpui-mcp](https://github.com/themixednuts/gpui-mcp) 的桥接，供 agent 经 MCP 读元素树、按元素点击和输入、截图（[ADR 0056](../../docs/adr/0056.md)）。

- 层：tool。一个单独的 Cargo 工作区，有自己的 `Cargo.lock`，被根 `Cargo.toml` `exclude`。不进安装包。
- 设计文档：[app 设计「自动化构建的装配」](../../docs/design/app.md#自动化构建的装配)
- issue：[#310](https://github.com/SoloJiang/lectern-app/issues/310)

## 边界

- `[patch.crates-io]` 把 `gpui-pre` 换成 gpui-mcp 仓库里打过补丁的版本，只在本工作区生效。主工作区和发布构建不打补丁。
- gpui-mcp 的提交只写在 `Cargo.toml` 里（依赖和 `[patch]` 两处，一起改）；`just automation-server-install` 从 `Cargo.lock` 读出它。
- 只经 `lectern_app::run_with` 的回调接入：首帧之后装桥接，不改 `app` 的启动顺序。日志只记错误的种类，不记路径和令牌。
- 主工作区升级 GPUI 后，这里的版本、`Cargo.lock` 和 gpui-mcp 的提交要一起更新；gpui-mcp 没有目标版本的补丁时不升级（ADR 0056）。
- CI、`just check`、`just test`、`just deny` 都不覆盖本工作区，它的依赖没有许可证和安全公告检查（ADR 0056「已知限制」）。

## 使用

```sh
just automation-server-install   # 第一次：把 MCP 服务端装到 target/automation/server
just automation                  # 构建并在后台启动，HOME 是 target/automation/home
```

仓库根的 `.mcp.json` 登记了服务端 `lectern-gpui`（`just automation-server`）。端点目录是 `/tmp/lectern-mcp-<uid>`（套接字路径不能超过 104 字节），由 `scripts/automation_endpoints.sh` 检查。截图经 xcap，需要屏幕录制权限。

## 测试

没有自动测试。`app` 的 `src/tests/hooks.rs` 测回调的调用位置；本工作区的验收是手动的（app 设计「验收」）。
