# 任务清单：issue-12-support

## 概述

实现 FRP TLS 开关：数据模型 → toml 生成 → UI → 测试。

> **二元禁令（零容忍）**：禁止未替换的模板占位符、TODO 标记、省略号占位。

---

## 交付物清单（Scope-lock）

- **预计新建文件数**: 0 个（规格 3 个已建）
- **预计修改文件数**: 10 个
- **预计新增/修改函数数**: 约 6 个
- **交付物逐项列举**:
  1. `FrpProfile.tls_enable` / `TunnelConfig.frp_tls` / `ActionsConfig.frp_tls`
  2. `FrpServerConfig.tls_enable` + `append_frpc_transport_settings` TLS 行
  3. FRP Profile DTO/API 与设置页勾选
  4. `TunnelConfigForm` Legacy TLS 勾选与工作区保存映射
  5. 隧道配置比较纳入 `frp_tls`
  6. toml 生成单测

---

## 任务列表

### 阶段 1: 准备工作

- [ ] 1.1 确认 `append_frpc_transport_settings` 与 Profile/隧道表单挂载点
  - **证据块**: `src-tauri/src/tunnel/frp/mod.rs` 中 `append_frpc_transport_settings` 现写入 loginFailExit/heartbeat；`TunnelConfigForm.svelte` 已有 `cloudflare_http2` 勾选模式
  - **涉及文件**: 只读确认，无代码变更
  - _需求: FR-1, FR-2_ ｜ _设计: 技术方案_

---

### 阶段 2: 核心实现

- [ ] 2.1 为 Profile 与 Tunnel/Actions 增加 TLS 布尔并默认 false
  - **证据块**: `settings/model.rs` 的 `FrpProfile`；`workspace/model.rs` 的 `TunnelConfig`/`ActionsConfig` 及 default 构造
  - **涉及文件**: `src-tauri/src/settings/model.rs`（+5）；`src-tauri/src/workspace/model.rs`（+15）
  - _需求: FR-1, FR-2, NFR-1_ ｜ _设计: 数据模型_

- [ ] 2.2 扩展 `FrpServerConfig` 与 toml 生成写入 TLS 三行
  - **证据块**: `frp_server_config` 组装 `FrpServerConfig`；`append_frpc_transport_settings`
  - **涉及文件**: `src-tauri/src/tunnel/frp/mod.rs`（+40）
  - _需求: FR-1, FR-3_ ｜ _设计: 架构设计_

- [ ] 2.3 打通 Profile DTO、设置页与隧道表单/工作区保存
  - **证据块**: `commands/frp_profiles.rs` DTO；`settings/frp/+page.svelte`；`TunnelConfigForm.svelte`；`workspace/[id]/+page.svelte`；`commands/tunnel.rs` 配置比较
  - **涉及文件**: 上述路径与 `src/lib/api/settings.ts`、`src/lib/types.ts`（合计 +80）
  - _需求: FR-1, FR-2_ ｜ _设计: 文件结构_

---

### 阶段 3: 集成测试

- [ ] 3.1 添加 toml TLS 单测并跑 `cargo test --lib` 相关过滤
  - **证据块**: `tunnel/frp/mod.rs` 现有 `build_frpc_toml_uses_global_profile_server` 测试
  - **涉及文件**: `src-tauri/src/tunnel/frp/mod.rs` tests
  - _需求: FR-1, FR-3, NFR-3_ ｜ _设计: 测试策略_

---

## 检查点

- [ ] 阶段 1 完成后：挂载点确认无歧义
- [ ] 阶段 2 完成后：tls true 生成含 TLS 三行；false 不含；Profile/Legacy UI 可用
- [ ] 阶段 3 完成后：相关单测通过

---

## 需求覆盖矩阵

| 需求 ID | 设计章节 | 任务编号 | 状态 |
|---------|----------|----------|------|
| FR-1 | 数据模型 / 架构 | 2.1, 2.2, 2.3, 3.1 | 未开始 |
| FR-2 | 数据模型 / 文件结构 | 2.1, 2.3 | 未开始 |
| FR-3 | 架构设计 | 2.2, 3.1 | 未开始 |
| NFR-1 | 数据模型 | 2.1 | 未开始 |
| NFR-3 | 测试策略 | 3.1 | 未开始 |

---

## 文件变更清单

| 文件 | 操作 | 行数预算 | 说明 |
|------|------|----------|------|
| src-tauri/src/settings/model.rs | 修改 | 10 | Profile.tls_enable |
| src-tauri/src/workspace/model.rs | 修改 | 20 | frp_tls 字段与默认 |
| src-tauri/src/tunnel/frp/mod.rs | 修改 | 60 | 生成与测试 |
| src-tauri/src/commands/frp_profiles.rs | 修改 | 15 | DTO |
| src-tauri/src/commands/tunnel.rs | 修改 | 10 | 比较 frp_tls |
| src/lib/api/settings.ts | 修改 | 10 | 类型 |
| src/lib/types.ts | 修改 | 10 | 类型 |
| src/lib/components/TunnelConfigForm.svelte | 修改 | 40 | Legacy 勾选 |
| src/routes/settings/frp/+page.svelte | 修改 | 30 | Profile 勾选 |
| src/routes/workspace/[id]/+page.svelte | 修改 | 20 | 保存映射 |

---

## 检查清单

- [x] 交付物清单（Scope-lock）已填
- [x] 每条任务标题是动词+对象+约束
- [x] 每条任务含证据块
- [x] 每条任务标注涉及文件与行数预算
- [x] 任务分阶段合理
- [x] 每条任务都回链到 FR 与 design 章节
- [x] 需求覆盖矩阵已填
- [x] 阶段 3 包含对照验收标准核验
- [x] 全文无占位符
