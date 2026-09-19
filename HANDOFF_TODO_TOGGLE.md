# 交接文档：Todo 勾选切换（Core 领域方法 + Web 端点 + 可点击面板）

> 交接分支：`feat/react-web-ui`，起点 commit `a2529d0`（工作区干净）。
> 这是一个**独立问题**，与该分支其余改动解耦，可单独实现、单独提交。

## 1. 背景与目标

Web UI 右栏 Todos 面板的勾选框当前是 `disabled` 只读的（原始阶段任务书曾禁止
实现 todo mutation，因为当时没有契约）。用户现已明确要求：**勾选框必须可点击
切换完成状态**。同时用户裁定（本人认同）：该能力应由 **Core 以领域方法提供**，
而不是表现层穿透到 Storage。

## 2. 架构决定（已与用户对齐，不要重新讨论）

- **Core `Workspace` 增加领域方法** `toggle_todo_task(index: usize) -> Result<bool>`，
  一行委托到既有的 `Storage::toggle_todo_task`。理由：
  - 这是带业务语义的领域操作（任务序号契约、跨日记文件定位与重写），
    且被 TUI 与 Web 两个表现层共同需要（同 job-delivery 收归 core 的先例）；
  - web 层对 `workspace.storage()` 只应读不应写；现在唯一一处写穿透
    （本交接撤销的草稿）就是反面教材；
  - 实现成本与 `Workspace::todos()` 相同的一行委托，`append_to_today`
    也已采用同样形状。
- 注意：旧任务书的"todo 只读"禁令已被用户此请求显式取代，无需再顾虑。

## 3. 现状盘点（已核实）

- **Core 已有能力**：`Storage::toggle_todo_task(index: usize) -> Result<bool>`
  （`src/core/storage.rs` ~L888）。语义：按 `load_todo_tasks()` 完全相同的
  顺序（daily 文件**最新日期优先**、文件内任务行顺序）找到第 `index` 个任务行，
  翻转 `[ ]`/`[x]` 并重写该 daily 文件；找不到返回 `Ok(false)`。
- **TUI 用法参考**：`src/tui/app/documents/todos.rs:51`
  `self.storage().toggle_todo_task(index)` —— 重构后 TUI 也应改调
  Workspace 方法（可选，顺手做；不阻塞 web 部分）。
- **`GET /api/todos`**（`src/web/workspace.rs::todos`）返回 `Vec<TodoDto>`
  `{checked, text}`，顺序与 toggle 的 index 契约**完全一致**，前端列表的
  数组下标即可用作 toggle index。
- **已撤销的草稿**：我曾以
  `state.workspace.storage().toggle_todo_task(index)` 直接在 handler 里实现过
  一版（连同路由与测试），已按本决定还原。其 HTTP 形状可复用，见 §5 参考。

## 4. 任务分解

### 4.1 Core（1 行 + 测试）
```rust
// src/core/workspace.rs，紧挨 todos()/append_to_today()：
/// Flip the completion state of the indexed daily task (same ordering as
/// [`Workspace::todos`]). Returns whether a task was toggled.
pub fn toggle_todo_task(&self, index: usize) -> Result<bool> {
    self.storage.toggle_todo_task(index)
}
```
建议补一个 Workspace 级测试（模仿 `web/tests.rs` 里 daily 相关测试的
workspace 构造方式即可，或直接依赖 4.3 的 HTTP 测试覆盖）。

### 4.2 Web 端点（`src/web/workspace.rs` + `src/web/server.rs` 路由）
- 路由：`POST /api/todos/{index}/toggle`（`axum::extract::Path<usize>`，
  非 usize 自动 400）。
- Handler：调用 `state.workspace.toggle_todo_task(index)`：
  - `Ok(false)` → `ApiError::NotFound(format!("no task at index {index}"))`；
  - `Ok(true)` → 重新 `state.workspace.todos()`，返回刷新后的
    `Json<Vec<TodoDto>>`（省一次往返）。
- **注意 handler 不得再出现 `workspace.storage()`** —— 这是本任务的核心
  修正点。

### 4.3 Web Rust 测试（`src/web/tests.rs`）
临时工作区内 `storage.append_to_today("- [ ] first task")` ×2，然后：
- `POST /api/todos/0/toggle` → 200，`json[0].checked == true`、
  `json[1].checked == false`；
- `POST /api/todos/9/toggle` → 404，`error_code == "not_found"`；
- 再 toggle 一次 → 恢复 `checked == false`。
（现成的 `post_json`/`request` helper 与 `daily_today_append_creates_then_appends`
测试可作模板。）

### 4.4 前端（`web/`）
- `src/api/workspace.ts`：`toggleTodo: (index: number) => apiPost<TodoItem[]>(`/api/todos/${index}/toggle`)`。
- `src/queries/workspace.ts`：`useToggleTodo()` mutation：
  - `onSuccess: (fresh) => queryClient.setQueryData(workspaceKeys.todos(), fresh)`
    并 `invalidateQueries(workspaceKeys.daily())`（toggle 重写了 daily 文件内容）
    与 `workspaceKeys.document(note.path)`（若该日记正在编辑器中打开，让
    编辑器走 content_changed → 自动刷新；watcher 也会兜底广播，此步可选）。
- `src/features/todos/TodoPanel.tsx`：
  - 移除 checkbox 的 `disabled`，新增 `onChange` → `useToggleTodo(index)`；
  - `disabled={toggle.isPending}`（防止并发双击）；
  - 视觉：`cursor-pointer`、hover 反馈；文本划线样式按 `checked` 保留；
  - **index 即列表下标**，`li` 渲染顺序不可重排（当前实现天然满足）。
- 测试：`TodoPanel.test.tsx` 增加一条"点击 checkbox 调用 toggle"的组件测试
  （mock mutation 或断言请求），以及现有两条测试去掉 `disabled` 断言。

### 4.5 验证清单
```bash
cargo fmt --check && cargo clippy --all-targets && cargo test --lib
cd web && pnpm typecheck && pnpm test && pnpm build
```
浏览器 smoke：右栏点勾选框 → 划线状态翻转 → Daily 卡片河里对应任务行同步
勾选（watcher content_changed → /daily 缓存失效）→ 状态栏无报错。

## 5. 参考：已验证可用的 HTTP 形状（撤销稿摘录）

```rust
pub async fn toggle_todo(
    State(state): State<WebState>,
    axum::extract::Path(index): axum::extract::Path<usize>,
) -> ApiResult<Json<Vec<TodoDto>>> {
    let toggled = state.workspace.toggle_todo_task(index).map_err(ApiError::Internal)?;
    if !toggled {
        return Err(ApiError::NotFound(format!("no task at index {index}")));
    }
    let items = state.workspace.todos().map_err(ApiError::Internal)?;
    Ok(Json(items.iter().map(TodoDto::from_todo_item).collect()))
}
```

## 6. 环境备注

- 38121 上可能挂着一个**开发用 nole web（fixture 工作区 /tmp/nole-smoke）**，
  是交接前验证遗留：`pkill -f "nole web"` 后自行 `nole web` 或
  `NOLE_DIR=... cargo run -- web` 即可。
- 前端验证时 IAB 对**功能键**（Enter/Esc）的事件派发不可靠，字符输入正常：
  交互断言请在 jsdom 渲染测试里做，浏览器只做视觉确认。
