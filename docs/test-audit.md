# 测试审查记录

审查日期：2026-11-14。基线为 Web 端移除后的 93 个含测试的文件、1007 项测试。

本轮完成全量清单、断言结构和相似度扫描，对文案锁定、Schema 字段复述、重复覆盖、内部身份断言及名称与实际覆盖不一致的候选项进行源码复核，随后完成行为重写、完整回归与受控缺陷验证。相似度用于定位候选项，最终取舍依据实际保护的行为。

## 结果

移除或合并 15 项独立测试；原客户端测试改为两条公共 API 行为测试。测试总数从 1007 调整为 993。数量用于说明变更范围，取舍依据每项测试保护的契约。

- 计算器、任务计划和 HTTP 工具通过真实注册入口验证输入与结果，覆盖合法边界、错误类型、缺失字段和额外字段。
- 任务计划在请求失败后保持原值；合法的数量和内容长度边界可执行。
- 客户端从空目录启动，实际文件变更经过监听进入搜索索引；真实 Agent 运行失败传递给独立订阅者，历史可重新打开。
- 寄存器测试修改读取结果后再次读取，验证内容可以重复使用。
- 会话渲染依据独立预期文本检查内容完整性、出现次数和顺序。
- 行窗口测试明确验证可见范围；工具时间线检查行数、内容和状态，分配地址交由实现选择。
- 界面颜色断言引用语义主题字段，删除测试专属的重复默认色板。
- 计算器长度、任务数量和任务内容长度由同一组生产常量供 Schema 与执行逻辑引用；测试使用需求边界验证行为。

## 移除与合并依据

| 原测试 | 处理依据 |
|---|---|
| `system_prompt_stays_compact` | 任意的静态文案字节预算，交由提示词审阅。 |
| `system_prompt_identifies_the_compiled_platform_shell_and_initial_pty_size` | 逐句复述提示词；平台、shell 和 PTY 行为另有功能测试。 |
| `system_prompt_describes_the_agent_maintained_workspace` | 逐句复述提示词；工作区边界由文件工具测试验证。 |
| `system_prompt_explains_persistent_memory_maintenance` | 静态提示词措辞审阅。 |
| `system_prompt_prefers_built_in_tools_before_shells` | 静态提示词措辞审阅；shell 限制由执行入口测试验证。 |
| `tavily_response_is_rendered_as_agent_relevant_text` | 与工具模块的响应渲染覆盖合并；补入请求元数据过滤断言。 |
| `default_file_reproduces_the_current_theme` | 重复定义默认色板；保留主题解析、文件加载和独立语义字段测试。 |
| `retry_count_is_not_part_of_the_persisted_session_schema` | 单独检查历史字段缺席；已有持久化测试验证运行状态的保存与恢复。 |
| `provider_implementations_are_send_sync_and_object_safe` | 生产代码的 Provider 类型约束已由编译器检查；局部数组长度断言无功能价值。 |
| `backlink_hitbox_covers_exactly_the_rendered_truncated_name` | 合入实际渲染文本与点击区域一致的集成测试。 |
| `opens_and_serves_clients_without_terminal_dependencies` | 由公共 API 客户端测试负责启动、文件监听、索引和会话事件。 |
| `pinned_version_matches_the_vendored_engine` | 移除版本字面量复述；内联资源安全检查合入资源嵌入测试。 |
| `bootstrap_uses_locked_down_render_options` | 导出 HTML 集成测试已经检查实际嵌入的初始化配置。 |
| `subscriptions_are_independent_receivers` | 原测试直接向内部 Tokio 通道发送事件；改由公共客户端测试观察真实运行失败。 |
| `permission_mode_round_trips` | 简单设置和读取已由 TUI 模式切换与会话持久化行为覆盖。 |

## 保留原则

持久化内容、路径保护、审批与取消、流式协议、并发顺序、重试预算、离线导出资源及其安全约束、UI 操作结果、选择指示器与窗口滚动规则均保留行为覆盖。不同输入即使使用相似断言，也可以保护不同边界，例如 URL 图片与附件图片、本机移动与跨设备移动。

核心层和客户端层的相近测试分别验证领域规则与接线行为；保留这类跨层覆盖。缓存和虚拟列表测试可以检查明确的工作量、容量和失效边界。窄窗口的无崩溃测试以成功渲染为观察结果。输入 Schema 的测试可以通过实际校验验证互斥字段和秘密输入保护。

提示词内容的质量通过代码审阅判断。测试保留动态项目指令和记忆内容的注入、顺序与缓存隔离契约。

## 受控缺陷验证

每次只临时改变一处生产逻辑，运行对应测试并确认行为断言失败，然后恢复源码。五项检查均捕获了缺陷。

| 临时缺陷 | 捕获它的测试 |
|---|---|
| 跳过注册入口的输入校验 | `registered_todo_rejects_invalid_requests_and_preserves_the_plan` |
| 读取寄存器时移除其内容 | `editing_loaded_lines_preserves_reusable_register_contents` |
| 表达式长度上限从 1000 放宽为 1001 | `registered_calculator_accepts_the_boundary_and_rejects_invalid_requests` |
| 应用轮询返回空事件，停止转发文件变更 | `fresh_workspace_supports_document_edits_and_indexes_external_changes` |
| 可见行窗口向后偏移一行 | `line_window_returns_the_requested_rows_and_handles_the_end` |

这五项实验验证所选行为断言的缺陷检测能力；全套回归用于检查本轮变更的集成结果。

## 全量文件清单

下表覆盖基线的全部 93 个测试文件。数量包含 `.inc` 中的内联测试；测试函数内的多个输入案例仍计为一项。

| 文件 | 原测试数 | 当前测试数 | 本轮处理 |
|---|---:|---:|---|
| `src/core/agent/activity.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/collaboration/tests.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/context.rs` | 6 | 6 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/hashline/block.rs` | 18 | 18 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/hashline/parser.rs` | 24 | 24 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/hashline/plan.rs` | 34 | 34 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/hashline/rebase.rs` | 9 | 9 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/hashline/registers.rs` | 6 | 6 | 改为修改读取结果后验证再次读取 |
| `src/core/agent/images.rs` | 8 | 8 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/jobs.rs` | 11 | 11 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/results.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/shell_helper.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/shell_policy.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/snapshots.rs` | 16 | 16 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/subagent.rs` | 8 | 8 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/terminal.rs` | 9 | 9 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tests_part1.inc` | 53 | 53 | HTTP 注册与调度契约保留；Schema 行为交给注册入口测试 |
| `src/core/agent/tests_part2.inc` | 45 | 39 | 清理提示词文案、合并渲染重复覆盖、收敛注册断言 |
| `src/core/agent/tool_stream.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/attachment_ops.rs` | 12 | 12 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/calculator.rs` | 6 | 6 | 实际注册入口与长度边界 |
| `src/core/agent/tools/file_edit.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/file_ops.rs` | 12 | 12 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/file_patch.rs` | 10 | 10 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/grep.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/interactive.rs` | 4 | 4 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/jobs.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read.rs` | 9 | 9 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/attachments.rs` | 6 | 6 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/document.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/documents.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/paging.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/skill.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/text.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/read/web.rs` | 7 | 7 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/shell.rs` | 14 | 14 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/todo.rs` | 3 | 3 | 实际注册入口、边界与失败后计划保持 |
| `src/core/agent/tools/util.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/wait.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/web.rs` | 21 | 21 | 实际注册入口；合并搜索结果元数据过滤 |
| `src/core/agent/tools/workspace_quota.rs` | 9 | 9 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent/tools/write_policy.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/agent_panel.rs` | 8 | 8 | 移除地址断言；名称对应运行失败的可见结果 |
| `src/core/agent_service.rs` | 4 | 2 | 通道与简单设置测试由客户端行为覆盖 |
| `src/core/agent_session.rs` | 8 | 7 | 保留实际持久化与恢复；清理历史字段断言 |
| `src/core/agent_sessions/tests.rs` | 5 | 5 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/attachment.rs` | 28 | 28 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/attachment_index.rs` | 6 | 6 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/attachment_usage.rs` | 7 | 7 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/document_index.rs` | 5 | 5 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/embedded_terminal.rs` | 5 | 5 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/export.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/export/assets.rs` | 10 | 10 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/export/highlight.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/export/html.rs` | 21 | 21 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/export/katex.rs` | 3 | 1 | 合并内联资源安全断言；保留离线字体嵌入 |
| `src/core/image_data.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/markup.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/nole.rs` | 1 | 0 | 启动覆盖移至公共客户端测试 |
| `src/core/provider/completions.rs` | 7 | 7 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/provider/messages.rs` | 8 | 8 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/provider/mod.rs` | 2 | 1 | 保留传输超时行为；编译约束交由编译器检查 |
| `src/core/skill.rs` | 7 | 7 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/storage.rs` | 50 | 50 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/storage/atomic.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/wiki_link_index.rs` | 4 | 4 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/workspace.rs` | 25 | 25 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/workspace_index.rs` | 7 | 7 | 保留所属功能、协议或边界的行为覆盖 |
| `src/core/workspace_watcher.rs` | 9 | 9 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/dialogs/handlers.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/documents/attachments.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/sessions.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/skill_tests.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/tests.rs` | 142 | 142 | 工具时间线保留内容与状态断言，清理地址断言 |
| `src/tui/app/text_input.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/app/wiki_completion.rs` | 5 | 5 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/backend.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/clipboard.rs` | 4 | 4 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/markdown.rs` | 29 | 29 | 颜色断言引用语义主题字段 |
| `src/tui/markdown/math.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/media.rs` | 15 | 15 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/notification.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/runtime.rs` | 10 | 10 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/terminal_keys.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/theme.rs` | 6 | 5 | 清理默认色板复述与重复测试色板 |
| `src/tui/ui/chat.rs` | 22 | 22 | 独立预期内容与顺序；清理地址断言 |
| `src/tui/ui/compose.rs` | 1 | 1 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/ui/document.rs` | 4 | 3 | 截断宽度由实际渲染集成测试负责；保留裁剪与滚动边界 |
| `src/tui/ui/skill_tests.rs` | 2 | 2 | 保留所属功能、协议或边界的行为覆盖 |
| `src/tui/ui/tests.rs` | 103 | 103 | 语义主题、准确的可见窗口测试命名 |
| `src/tui/vlist.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `tests/cli.rs` | 3 | 3 | 保留所属功能、协议或边界的行为覆盖 |
| `tests/core_client.rs` | 1 | 2 | 空目录启动、真实文件变更传播、运行失败事件与历史恢复 |

## 验证结果与入口

默认配置 993 项测试通过，纯核心配置 626 项测试通过；两种配置的严格 Clippy、格式检查与补丁空白检查通过。

```sh
cargo test --locked
cargo test --locked --no-default-features
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --no-default-features --all-targets -- -D warnings
cargo fmt --all --check
```
