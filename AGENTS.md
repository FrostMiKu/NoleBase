# Project Status

Nole has not been released. Prefer the best current API and data-model design over backward compatibility. Do not add compatibility layers, deprecated aliases, schema fallbacks, or migrations for interfaces that have only existed during development unless the user explicitly requests them.

When writing or modifying Rust code, keep the non-test code in every .rs file under 1500 lines. Necessary inline test code does not count toward this limit, including `#[cfg(test)]` modules, test functions, and test-only helpers that are appropriately colocated with the implementation. If the non-test code approaches this limit, proactively split functionality into submodules (mod) to keep files short and single-purpose.

For full-view selectable list UIs, always reserve a blank row above the first item and draw the shared vertical selection indicator across the complete selected area. Compact inline popups that float over content (for example the `[[` wiki completion popup above the compose input) are exempt from the blank row: they render tightly, but still reserve a one-cell gutter so the shared selection indicator marks the row without covering the label. Cover the applicable invariants with renderer tests.

For selectable list UIs whose window is fixed-height and scrolls (the command palette, the `[[` wiki completion popup, and any future equivalent), the selection leads and the list follows only at the edge it reaches. Keep the selection index and the scroll offset (first visible row) as two separate persistent state fields on the owning state. Up/Down keys move only the selection, clamped at both ends — never wrap. Reconcile the scroll offset from the selection at render time via `selection_viewport_start` and write it back, so the list scrolls by exactly one row only when the selection reaches the window's bottom edge (moving down) or its top edge (moving up); while the selection still fits inside the window it moves within the window and the list stays put. Never fix the selection in place and scroll the list directly to keep it visible. Cover the "selection leads, list follows at the edge" invariant with a renderer test that moves the selection up from a scrolled-down state and asserts the window did not follow.

# RULE

- 使用正向表述，禁止“否定之否定”的表述.
- 面向终端用户界面的一切表述文案如无必要不应描述内部实现细节。
- 测试逻辑而非实现。
- 单文件代码量除去必要的内联测试外不得超过 1500 行代码，超过了按逻辑和模块进行拆分。
- 遵守单一事实原则，同一份逻辑或数据只在一个权威位置实现和定义，其他代码一律调用或引用，不重复实现。
- 面向可维护的软件工程编写代码，而非仅仅只是实现功能。
- 除非明确要求，保持干净的最新实现，不保留旧版兼容代码，不留历史包袱。
- 名词一致性原则，对于同一实体概念，在文本或代码中始终使用同一名词，不随意换用同义词或别名，避免歧义。

# Testing

Use tests to protect observable behavior, persistence, protocol contracts, safety invariants, and explicit performance bounds. Exercise production entry points when checking integration. Keep expected results independent of the implementation under test; review static prose and allocation choices through code review. Place each invariant's authoritative test in the layer that owns it, and use cross-layer tests to verify integration behavior.
