# 需求文档：issue-12-support

## 功能概述

为 FRP 隧道增加 TLS 开关，使校园网/公司网等无法明文连接 frps 的环境可通过 `transport.tls.enable` 正常建连。优先提供开关；完整自定义 frpc.toml 编辑器本次不做。

## 历史经验与坑（来自记忆库）

- **可复用经验**: 隔夜断网修复已在 `append_frpc_transport_settings` 写入 `loginFailExit`/`heartbeat`；TLS 行应追加到同一处，避免分叉生成逻辑。Cloudflare HTTP/2 开关模式（工作区布尔 + 表单勾选）可复用到 FRP TLS。
- **必须规避的坑**: 多路由共用一个 frpc 进程时，TLS 必须与 serverAddr/port 一样取自共享连接配置；不得让同一进程的不同 proxy 产生冲突的 transport 段。

## 术语定义

- **FRP TLS**: frpc 客户端 `transport.tls.enable = true`，并配合 `transport.protocol = "tcp"` 与 `transport.tls.disableCustomTLSFirstByte = true`（与 issue #12 建议一致）。
- **全局 FRP Profile**: 设置页中的共享 frps 配置（地址/端口/token）。
- **Legacy 手动 FRP**: 工作区不选 Profile、直接填 server 的模式。

---

## 范围边界

**In Scope（本次要做）**
- 全局 FRP Profile 增加 `tls_enable` 开关，并在生成的 `frpc.toml` 中写入 TLS 相关项
- 工作区 MCP/Actions 隧道在 Legacy 手动 FRP 模式下提供 `frp_tls` 开关
- 选用 Profile 时以 Profile 的 `tls_enable` 为准；UI 展示只读说明
- 单元测试覆盖 toml 生成含/不含 TLS
- 隧道配置变更检测纳入 TLS，切换后触发既有重启路径

**Out of Scope（本次不做）**
- 完整自定义 frpc.toml 编辑器 / 任意 snippet 合并
- 自定义证书路径、mTLS、非 TCP 传输协议
- 修改 Cloudflare 隧道行为

---

## 需求列表

### FR-1: Profile 级 TLS 开关

**优先级:** Must  
**用户故事:** 作为在受限网络下使用共享 frps 的用户，我想在 FRP Profile 上打开 TLS，以便所有选用该 Profile 的工作区自动用 TLS 连接。

#### 验收标准（EARS）

1. WHEN 用户在设置 → FRP 中保存 Profile 且 `tls_enable=true` THEN 系统 SHALL 持久化该字段且重启后仍为 true
2. WHEN 工作区选用该 Profile 并启动 FRP 隧道 THEN 生成的 `frpc.toml` SHALL 包含 `transport.tls.enable = true`、`transport.protocol = "tcp"`、`transport.tls.disableCustomTLSFirstByte = true`
3. WHEN `tls_enable=false`（默认）THEN 系统 SHALL 不写入上述 TLS 行，且保留现有 `loginFailExit`/`heartbeat` 行为

### FR-2: Legacy 手动 FRP 的工作区 TLS 开关

**优先级:** Must  
**用户故事:** 作为未使用全局 Profile、直接填写 frps 地址的用户，我想在工作区隧道表单打开 TLS，以便同样能连上要求 TLS 的服务器。

#### 验收标准（EARS）

1. WHEN 隧道类型为 frp 且未选择 Profile THEN UI SHALL 展示「启用 FRP TLS」开关并绑定 `frp_tls`
2. WHEN `frp_tls=true` 启动隧道 THEN `frpc.toml` SHALL 含与 FR-1 相同的 TLS 行
3. WHEN 已选择 Profile THEN UI SHALL 不提供可编辑的工作区 `frp_tls`（以 Profile 为准），并可提示当前 Profile 是否启用 TLS

### FR-3: 多路由共用进程一致性

**优先级:** Must  
**用户故事:** 作为同时跑 MCP+Actions FRP 的用户，我希望共用 frpc 进程时 TLS 配置一致，以免连接失败。

#### 验收标准（EARS）

1. WHEN 多条路由共享同一 frpc 进程 THEN 系统 SHALL 只生成一段 transport 配置，TLS 取自该连接的 `FrpServerConfig.tls_enable`
2. WHILE 生成 toml THEN 系统 SHALL 继续写入 `loginFailExit = false` 与现有 heartbeat 设置

---

## 非功能需求

- **NFR-1（兼容性）**: 既有工作区/Profile JSON 缺省字段时 `tls_enable`/`frp_tls` 默认为 false，行为与升级前一致
- **NFR-2（安全）**: 不在日志中额外打印 token；TLS 仅控制传输开关，不改变认证模型
- **NFR-3（可测性）**: toml 生成可用纯函数单测验证，无需启动真实 frpc

---

## 依赖关系

- `src-tauri/src/tunnel/frp/mod.rs` 的 `build_frpc_toml*` / `append_frpc_transport_settings`
- `FrpProfile`、`TunnelConfig`、`ActionsConfig` 持久化与前端 `TunnelConfigForm` / `settings/frp`
- 既有隧道监督与重启逻辑（`commands/tunnel.rs` 配置比较）

---

## 检查清单

- [x] 已消化记忆库的历史经验，并逐条规避「历史坑」
- [x] 需求覆盖核心场景与边界场景
- [x] 每条需求有唯一 ID（FR-n），将在 design.md / tasks.md 中被引用
- [x] 验收标准使用 EARS 格式且可测
- [x] 已标注优先级（MoSCoW）
- [x] 范围边界（In/Out of Scope）明确
- [x] 非功能需求明确、尽量可量化
- [x] 依赖关系完整
