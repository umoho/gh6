# AGENTS.md

## 提交前检查

修改 Rust 代码后，提交前请确保：

- **clippy** — `cargo clippy --all-targets` 无 warning
- **rustfmt** — 代码已格式化（`cargo fmt`）

具体工具不限，也可以借助编辑器插件或 pre-commit hook 自动完成。

## Commit 风格

采用 Conventional Commits 格式，提交消息使用英文：

```
<type>(<scope>): <message>
```

### Type

| type | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | 修复 bug |
| `refactor` | 重构（不改变功能） |
| `chore` | 杂务（依赖更新、配置等） |
| `docs` | 文档 |
| `test` | 测试 |

### Scope

| scope | 说明 |
|-------|------|
| `crawl` | 爬虫核心（调度、BFS） |
| `socket` | Unix socket 通信 |
| `cli` | 命令行接口 |
| `db` | 数据库层 |
| `analyze` | 分析模块 |
| `export` | 导出模块 |
| `github` | GitHub API 客户端 |

### 示例

```
feat(crawl): implement FollowCrawler
feat(socket): add watch event broadcast
feat(cli): add status --watch command
fix(db): handle unique constraint on edges insert
refactor(crawl): extract rate limiter into module
chore: update rusqlite to 0.32
docs: add DESIGN.md and TODO.md
```

## 代码风格

- Rust edition 2024
- `cargo fmt` 和 `cargo clippy` 通过后再提交
- 异步代码使用 `tokio`
- 错误处理使用 `thiserror` 定义错误类型，不用 `anyhow`

## 代码规范 — 文档注释

文档注释使用英文。

### Module

```rust
//! Module description.
//!
//! Detailed description (optional).
//!
//! # Submodules
//!
//! * `mod1` - description
//!
//! # Types
//!
//! * [`Type1`] - description
```

### Struct

```rust
/// Short description.
///
/// Detailed description (optional).
pub struct Example {
    /// Field description.
    pub field: Type,
}
```

### Enum

```rust
/// Short description.
pub enum Example {
    /// Variant description.
    Variant1,

    /// Variant description.
    Variant2 {
        /// Field description.
        field: Type,
    },
}
```

### Function / Method

```rust
/// Short description.
///
/// Detailed description (optional).
///
/// # Arguments
///
/// * `param` - description (unit, range if applicable)
///
/// # Returns
///
/// Description.
///
/// # Panics
///
/// Conditions that may cause a panic.
pub fn example(param: Type) -> ReturnType {}
```

### Const

```rust
/// Description.
const NAME: Type = value;
```

