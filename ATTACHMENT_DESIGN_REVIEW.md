# 附件与 Agent Workspace 设计复核

复核范围：附件存储、附件 URI、Agent 文件权限、引用索引、附件管理 UI、文件监听与生命周期。

当前基线：

- LSP workspace diagnostics：0 errors，0 warnings
- `cargo test --bin nole`：432 passed，0 failed
- 以下问题主要是现有测试未覆盖的安全、一致性和扩展性边界。

## P0：立即修复

### 1. 打开附件存在 Workspace symlink 越界写入

涉及：

- `src/app/documents/attachments.rs:123-129`
- `src/app/input/links.rs:137-145`

附件打开副本目前写入 Agent 可自由修改的 `workspace/main`，两处均使用类似：

```rust
fs::create_dir_all(&directory)?;
fs::write(&path, bytes)?;
```

Agent 可以提前创建：

```text
workspace/main/.nole-open -> /some/external/directory
```

也可以把预期输出文件创建成 symlink。随后用户点击附件时，`fs::write` 会跟随 symlink，可能覆盖 Workspace 外部文件。

#### 推荐修复

不要把应用生成的打开副本放进 Agent Workspace。新增应用管理区域：

```text
cache/
└── attachment-open/
```

要求：

- `cache/` 对通用 Agent 工具完全拒绝；
- 应用内部独占管理；
- 使用 `create_new` 或安全原子替换；
- 每层目录拒绝 symlink；
- 文件名使用完整 digest 或随机临时名；
- 应用退出或启动时可以清理；
- 把两个 materialize 实现合并成一个 `AttachmentMaterializer`。

仅在现有 Workspace 路径上补 canonicalize 仍然容易留下 TOCTOU 竞态。

### 2. 删除附件的引用保护不是强一致的

#### Agent 工具完全不检查引用

`src/agent/tools/attachment_ops.rs:424-448` 的 `delete_attachment`：

1. 读取 metadata；
2. 请求审批；
3. 验证 metadata/hash；
4. 直接调用 `store.remove()`。

它没有访问 `AttachmentReferenceIndex`，因此 Agent 可以删除仍被笔记引用的附件，留下失效 URI。

#### UI 删除依赖尚未就绪或可能过期的异步索引

`App` 初始化时使用：

```rust
attachment_refs: AttachmentReferenceIndex::default()
```

索引稍后才由后台线程构建并通过 `apply_attachment_index` 写入。用户如果在初始快照到达前打开 Attachment 视图，引用数会暂时全部显示为零，并可能允许删除。

即使初始索引已经到达，笔记修改事件和索引刷新之间仍有时间窗口。确认删除时虽然重新检查了 `self.attachment_refs`，但检查的仍然可能是旧快照。

#### 推荐修复

建立共享的 `AttachmentUsageHandle`：

```rust
struct AttachmentUsageSnapshot {
    revision: u64,
    ready: bool,
    references: AttachmentReferenceIndex,
}
```

删除集中到一个服务：

```rust
AttachmentManager::trash(id, expected_revision)
```

规则：

- index 未 ready：拒绝删除；
- snapshot revision 落后于最新文件事件：拒绝或同步刷新；
- UI 和 Agent 工具都走同一删除入口；
- 最终提交删除前再做一次权威引用检查；
- Agent runtime 接收共享 `AttachmentUsageHandle`，不能直接调用 `AttachmentStore::remove`。

对于当前文件数量，删除前同步扫描 `daily/data/archives` 也是可接受的保守方案。删除是低频破坏性操作，正确性优先于延迟。

## P1：近期优化

### 3. 附件没有容量限制，并且读取会完整分配内存

当前：

- import 没有单文件大小限制；
- AttachmentStore 没有总容量限制；
- Workspace 没有总容量限制；
- `read_object()` 返回完整 `Vec<u8>`；
- `read` 在分页前已经读取完整附件；
- copy-to-workspace 先完整读入内存；
- UI open 先完整读入内存；
- `list_attachments` 无分页，一次返回全部 metadata。

例如一个 10 GB 附件可以被流式导入，但随后一次 `read` 或 open 就会尝试分配 10 GB。

#### 推荐限制

起始值可以是：

```text
普通附件单文件：256 MiB
Workspace 单文件：64 MiB
Workspace 总量：512 MiB
附件总量：可配置，默认不硬限制但提供告警
Agent 文本读取上限：1 MiB
Agent 二进制读取：只返回 metadata
```

API 改为：

```rust
AttachmentStore::open(id) -> VerifiedAttachmentReader
AttachmentStore::copy_to(id, writer)
AttachmentStore::read_limited(id, max_bytes)
```

避免 `Vec<u8>` 作为主要接口。

`list_attachments` 应增加：

```json
{
  "query": "",
  "offset": 0,
  "limit": 50,
  "sort_by": "imported_at",
  "order": "desc"
}
```

### 4. 内容地址与展示名称被错误地绑定在一起

现在 metadata 是“一份内容一份全局 metadata”：

```rust
AttachmentMetadata {
    id,
    source,
    mime_type,
    imported_at,
}
```

相同字节第二次导入会复用第一次的 metadata。例如：

```text
第一次：logo.png
第二次：company-avatar.png
```

第二次导入返回的仍可能是 `logo.png`。生成 Markdown 时也使用存储中的 `metadata.source`，而不是本次请求的显示名称。

并发导入时还存在语义不一致：

- 文档说 first-import-wins；
- `publish` 中 metadata 使用普通 rename；
- 注释实际写的是 last-write-wins；
- 并发时最终名称不确定。

#### 推荐模型

内容寻址对象只保存确定性信息：

```rust
ObjectMetadata {
    id,
    size,
    detected_media_type,
}
```

以下内容不应成为对象身份的一部分：

- display name；
- import source；
- imported_at；
- 用户给出的别名。

展示名称应放在引用或导入结果中：

```markdown
![company-avatar](nole-attachment://sha256/...)
```

如果确实要保存导入历史，可以另建：

```rust
AttachmentImport {
    object_id,
    display_name,
    imported_at,
    provenance,
}
```

canonical URI 仍只指向 object。

### 5. MIME 只根据扩展名推断

`src/attachment.rs:544-583` 根据 source extension 推断 MIME。

这意味着：

- 文本文件命名为 `.png` 会被标记为图片；
- PNG 命名为 `.bin` 不会作为图片显示；
- `media_type` 参数只验证是否与扩展名推断相同，没有验证内容。

建议：

- 图片使用 `image::guess_format`；
- PDF、ZIP、GIF、PNG、JPEG 等检查 magic bytes；
- UTF-8 文本再结合扩展名区分 Markdown/JSON/plain；
- metadata 保存 `detected_media_type`；
- 扩展名最多作为 fallback 或 display hint。

### 6. 引用索引实际是字符串扫描，不是 Markdown AST

`src/attachment_index.rs:270-293` 搜索所有裸字符串：

```text
nole-attachment://sha256/<digest>
```

因此代码示例也会被视为真实引用：

````markdown
```text
nole-attachment://sha256/...
```
````

这会造成：

- 代码块中的示例阻止附件删除；
- HTML comment、转义文本等也可能算引用；
- “引用”的语义和实际可点击/可渲染节点不一致。

建议复用 MBDown parser，只索引：

- Markdown link destination；
- Markdown image destination；
- MBDown embed；
- `[link=...]` target。

如果希望裸 URI 也成为引用，应先让 renderer 把裸 URI 正式识别为链接，再统一索引语义。

目前也没有索引持久化 Agent session 里的附件 URI。需要明确产品决策：

- Agent session URI 也算强引用；或
- Agent session 只算弱引用，允许失效，但 UI 应提示。

建议 Agent session 使用弱引用：笔记是持久知识，Agent 历史不是附件保留依据。

### 7. Content-addressed Store 读取时只检查大小，不检查 hash

`read_object()` 当前只验证：

```rust
bytes.len() == metadata.size
```

如果对象内容被替换为相同长度，仍会在原 digest URI 下返回错误内容。

这违反内容寻址的核心不变量：

```text
sha256(bytes) == URI digest
```

建议：

- `read_object` 在读取过程中同时计算 hash；
- streaming copy/open 也通过 `VerifiedAttachmentReader` 验证；
- import 遇到已存在 object 时验证 hash，不能只检查是否存在；
- 可缓存 `{inode, mtime, size, verified}`，避免每次重新 hash；
- 首先保证正确，再考虑缓存。

### 8. Trash 不是原子对象

当前删除依次移动：

1. object；
2. metadata。

如果第二步失败，active store 会进入半删除状态。Trash 中 object 和 metadata 也不是一个原子单元，不便恢复。

建议布局调整为每附件一个目录：

```text
objects/<digest>/
├── content
└── metadata.json
```

删除变成单次目录 rename：

```text
objects/<digest> -> trash/<timestamp>-<digest>
```

收益：

- object + metadata 原子移动；
- restore 简单；
- trash listing 简单；
- 清空 trash 简单；
- 不会产生 object/metadata suffix 不一致。

若继续保持当前双目录布局，至少先写 tombstone transaction，再执行两步移动，并支持启动恢复。

### 9. Markdown display name 没有正确转义

`markdown_embed()` 直接插入：

```rust
format!("![{}]({uri})", metadata.source)
```

但 `validate_display_name` 只拒绝路径分隔符、NUL 和 `.`/`..`，仍允许：

- `]`
- `[` 
- 换行
- 控制字符
- 极长名称

例如名称：

```text
x](https://example.com).png
```

会生成破损或改变语义的 Markdown。

建议：

- Markdown label 使用专门 escape；
- 拒绝换行和控制字符；
- 限制名称字节长度；
- display name 不应来自全局对象 metadata，见前面的模型调整。

## P2：后续优化

### 10. Move 仍然总是 copy + delete

`src/agent/tools/file_ops.rs:444-458` 即使在同一文件系统也先复制再删除：

- 大文件慢；
- 需要双倍瞬时空间；
- 不是原子 move；
- 中途崩溃可能留下目标副本。

建议：

1. 同文件系统优先 `fs::rename`；
2. `EXDEV` 时才 copy + sync + remove；
3. copy 完成后 fsync 文件和父目录；
4. 审批后重新验证 source identity。

### 11. Watcher 仍监听整个 Nole root

虽然 `process_workspace_events` 会过滤 Workspace 事件，但 `notify` 仍递归 watch 整个 root。高频 Agent 操作仍会：

- 生成事件；
- 排队；
- 唤醒主循环；
- 然后才被过滤。

建议只 watch：

- `daily/`
- `data/`
- `archives/`
- `themes/`
- 指定 config 文件；
- 必要时 `attachments/metadata/`。

不要注册 `workspace/` watcher。

### 12. Attachment UI 默认按 digest 排序

`AttachmentStore::list()` 按 ID 排序。对用户来说 SHA-256 顺序没有意义。

建议默认：

```text
imported_at desc
```

可选：

- name；
- size；
- type；
- reference count；
- unreferenced first。

另有一个小错误：`referenced_locations()` 使用引用出现次数，却显示成 “N notes”。同一篇笔记引用三次会显示 “3 notes”；应使用 distinct locations 数量。

## 推荐执行顺序

### 第一批：安全和一致性

1. 把 open materialization 移出 Agent Workspace，消除 symlink 越界写。
2. 统一附件删除服务，让 UI 和 Agent 都做强一致引用检查。
3. 为 import/read/open/copy 增加容量限制和 streaming API。
4. `read_object` 验证实际 SHA-256。

### 第二批：数据模型

5. 拆开 content object 与 display name/provenance。
6. MIME 改为内容检测。
7. 引用索引改为 AST。
8. Trash 改成附件目录级原子 rename。

### 第三批：体验和性能

9. Move 优先原子 rename。
10. watcher 改成选择性监听。
11. list 分页、排序和 Workspace 配额。

## 总结

整体架构方向正确，主要问题是：

1. Agent 可写 Workspace 被同时用作应用安全缓存；
2. 引用保护只存在于 UI 的异步缓存层，而不在删除事务本身。

这两点应在继续扩展附件功能前优先修复。
