# 附件与 Agent Workspace 设计现状

复核范围：附件身份与存储、Agent 文件边界、引用索引、删除一致性、UI 选择器和生命周期。

当前验证基线（2026-08-22）：

- `cargo test --all-targets`：852 passed
- `cargo clippy --all-targets --all-features -- -D warnings`：通过
- Workspace diagnostics：0 errors，0 warnings

## 数据模型

附件采用稳定 UUID 身份，规范 URI 为：

```text
nole://attachment/<lowercase-hyphenated-uuid>
```

内容保持可变；更新内容会保留 UUID 与 URI。重复导入相同字节会生成两个独立附件，展示名称、来源和导入时间属于各自附件的 metadata。

每个附件对应一个私有目录：

```text
attachments/
├── <uuid>/
│   ├── content.<ext>
│   └── metadata.json
└── trash/
```

导入先写入唯一 staging 目录，完成同步后以一次目录 rename 发布。删除同样以一次目录 rename 移入 `trash/`，读者只会看到完整的活动附件。

## 内容与并发

- 单附件上限为 256 MiB，导入和更新均采用流式 I/O。
- MIME 优先根据 magic bytes 和 UTF-8 内容检测，扩展名作为补充信息。
- `size` 每次从活动内容文件重新读取，外部编辑会立即反映到 metadata 查询。
- checkout 返回 `sha256:<hex>` 内容 token；update 在发布前重新计算活动内容 token，以乐观并发控制保护用户和 Agent 的编辑。
- staged file 通过共享原子发布 primitive 替换活动内容；Unix 使用 `rename`，Windows 使用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`。

## Agent Workspace 边界

应用可以直接打开附件的真实内容文件。Agent 通过显式 checkout 把独立副本写入 `workspace/main`，编辑后携带 checkout token 调用 update。

Workspace 写入遵循以下不变量：

- 路径保持在 `workspace/main` 内；
- 每个已存在组件都经过真实路径与 symlink 校验；
- 目标使用 `create_new` 语义；
- 单文件容量上限为 64 MiB，Workspace 总容量上限为 512 MiB；
- 失败的复制会清理部分目标文件。

Agent 接口只暴露附件 ID、URI、metadata、内容 token，以及有界或流式读取能力；物理 `attachments/` 路径由应用管理。

## 引用索引与删除一致性

共享 `DocumentIndex` 读取 `daily/`、`data/` 和 `archives/`，以 MBDown AST 同时派生：

- hashtag 索引；
- wiki-link 索引；
- canonical attachment URI 索引。

代码块、HTML 注释、转义文本和普通文本保持内容语义，只有渲染器识别的 link、image、embed 和 `[link=...]` 目标进入附件引用索引。公共聚合器分别维护总出现次数与排序后的 distinct locations。

UI 和 Agent 共用 `AttachmentUsageHandle`。删除流程要求：

1. 初始权威快照已发布；
2. 调用方 revision 与当前快照一致；
3. 删除提交前同步扫描全部受管理笔记；
4. 权威扫描得到零引用；
5. 附件目录原子移入 trash。

快照只接受严格递增 revision；同步权威扫描会刷新同 revision 下的引用内容，从而覆盖文件事件与后台索引刷新之间的时间窗口。

## UI 不变量

附件、标签、搜索和其他 selectable list 共用选择区域几何：

- 第一项上方保留一行空白；
- 选择背景覆盖共享区域；
- 竖向选择指示器覆盖完整选区；
- metadata 前景色与 DIM 修饰在选择背景上保持可见。

Renderer tests 直接检查 buffer 中的空白行、背景、完整指示器和 metadata 样式。

## 缓存与恢复

文档索引 cache stamp 包含 mtime、size 和 SHA-256。哈希与解析使用同一份文件内容，确保同尺寸、同 mtime 的内容变化也会触发重建。

附件 trash 保留完整目录单元，为后续恢复和清理策略提供稳定边界。附件总容量当前由宿主存储空间决定；产品若需要配额，可在 `AttachmentStore` 的流式写入边界统一加入。
