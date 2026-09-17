# 设计文档：issue-12-support

## 概述

在现有 frpc.toml 生成管线中增加可选 TLS transport 段；数据落在 `FrpProfile.tls_enable` 与工作区 `frp_tls`，UI 对齐 Cloudflare HTTP/2 开关模式。

**对应需求:** FR-1, FR-2, FR-3, NFR-1, NFR-2, NFR-3

---

## 技术方案

### 技术选型

| 类别 | 选择 | 理由 | 关联需求 |
|------|------|------|----------|
| 配置落点 | Profile + Legacy 工作区布尔 | Profile 覆盖共享 frps；Legacy 保留手动 server | FR-1, FR-2 |
| toml 写入 | 扩展 `append_frpc_transport_settings` | 与 loginFailExit/heartbeat 同路径，避免重复生成 | FR-1, FR-3 |
| UI | 设置页 Profile 勾选 + 隧道表单 Legacy 勾选 | 复用 `cloudflare_http2` 交互 | FR-2 |

### 架构设计

```
UI (settings/frp | TunnelConfigForm)
    → WorkspaceProfile / FrpProfile JSON
        → frp_server_config(...).tls_enable
            → build_frpc_toml_for_routes
                → append_frpc_transport_settings(lines, tls)
                    → managed frpc.toml → frpc process
```

---

## 数据模型

| 实体/字段 | 类型 | 约束 | 说明 |
|-----------|------|------|------|
| FrpProfile.tls_enable | bool | default false | 全局 Profile TLS |
| TunnelConfig.frp_tls | bool | default false | MCP Legacy 手动 FRP |
| ActionsConfig.frp_tls | bool | default false | Actions Legacy 手动 FRP |
| FrpServerConfig.tls_enable | bool | 运行时 | 生成 toml 时使用 |

解析规则：若 `frp_profile_id` 命中 Profile，则 `tls_enable = profile.tls_enable`；否则取对应服务的 `frp_tls`。

---

## API 设计

| 方法/函数 | 路径/签名 | 入参 | 出参 | 关联需求 |
|-----------|-----------|------|------|----------|
| append_frpc_transport_settings | `fn(&mut Vec<String>, tls: bool)` | lines, tls | 副作用写入 | FR-1, FR-3 |
| frp_server_config | 现有签名 | profile, kind, settings | FrpServerConfig含 tls | FR-1, FR-2 |
| list/save_frp_profiles DTO | 现有命令 | tls_enable 字段 | DTO | FR-1 |

不新增 Tauri 命令。

---

## 文件结构

```
docs/specs/issue-12-support/{requirements,design,tasks}.md
src-tauri/src/settings/model.rs
src-tauri/src/workspace/model.rs
src-tauri/src/tunnel/frp/mod.rs
src-tauri/src/commands/frp_profiles.rs
src-tauri/src/commands/tunnel.rs
src/lib/api/settings.ts
src/lib/types.ts
src/lib/components/TunnelConfigForm.svelte
src/routes/settings/frp/+page.svelte
src/routes/workspace/[id]/+page.svelte
```

---

## 设计决策

### 决策 1: 本次不做任意自定义 toml（关联需求: FR-1 Out of Scope）

**问题**: Issue 同时提到自定义配置文件与 TLS 开关。

**选项**:
1. 做完整自定义 toml 编辑器
2. 先做 TLS 开关覆盖主诉求

**决策**: 选择 2

**理由**: 校园网/公司网场景由固定 TLS 三行即可解决；任意 toml 合并易破坏监督器写入的 proxies/heartbeat，风险高。

### 决策 2: Profile 优先于工作区 frp_tls（关联需求: FR-2）

**问题**: 选用 Profile 时两套布尔可能冲突。

**选项**:
1. OR 合并
2. Profile 命中则只用 Profile

**决策**: 选择 2

**理由**: Profile 表示共享服务器能力；避免工作区覆盖导致同 Profile 不同工作区连接参数不一致。

---

## 测试策略

- 单测：`build_frpc_toml` / `build_frpc_toml_for_routes` 在 tls true/false 下断言 TLS 三行与 heartbeat 并存
- 单测：缺省反序列化 Profile/TunnelConfig 时 tls 为 false
- 手测：设置 Profile TLS → 启动隧道 → 打开 managed `frpc.toml` 确认；Legacy 路径同样确认

---

## 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| 服务端未开 TLS 时客户端误开 | 中 | 默认 false；文案说明需服务端支持 |
| 多工作区同进程 TLS 不一致 | 低 | 监督器已要求同 server；tls 取自该连接配置 |

---

## 检查清单

- [x] 技术方案与现有架构一致
- [x] requirements.md 中每条 FR 都被本设计覆盖
- [x] 文件结构对照真实代码库，路径可定位
- [x] 数据模型 / 接口契约清晰
- [x] 关键设计决策已记录并关联需求
- [x] 测试策略可验证验收标准
