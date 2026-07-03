# AGENTS.md

## Commit 风格

采用 Conventional Commits 格式：

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
