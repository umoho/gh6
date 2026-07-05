# gh6 — GitHub Social Graph Explorer

基于六度分隔理论的 GitHub 社交图谱爬虫与分析工具。

## 架构

```
┌──────────────────────────────────────────────────────────┐
│              gh6d (守护) + gh6 (客户端)                    │
│                                                          │
│  ┌─────────────┐         ┌─────────────────────────────┐ │
│  │    gh6d     │ ──写入──→  ~/.local/share/gh6/gh6.db  │ │
│  │  (launchd / │         │        SQLite               │ │
│  │   systemd)  │         └──────────────┬──────────────┘ │
│  └──────┬──────┘                        │  直读           │
│         │   Unix socket                 │                │
│         │   ~/.local/share/gh6/gh6.sock │                │
│  ┌──────┴──────────┐         ┌──────────┴──────────────┐ │
│  │ gh6 run         │         │  gh6 analyze {sub}       │ │
│  │ gh6 pause       │         │  gh6 export              │ │
│  │ gh6 status      │         └─────────────────────────┘ │
│  └─────────────────┘                                     │
└──────────────────────────────────────────────────────────┘
```

- **gh6d**：守护进程，由 launchd (macOS) 或 systemd (Linux) 管理，启动后处于 IDLE 状态
- **gh6**：客户端 CLI，通过 Unix socket 向 gh6d 发送命令

## 命令体系

```
gh6d                         启动守护进程（由 launchd / systemd 管理，无需手动调用）
gh6 run                      开始 / 恢复爬取
gh6 pause                    暂停爬取（守护进程保持运行）
gh6 status [--watch]         查看进度 / 实时监控

gh6 analyze path <user>      查询从种子用户到目标用户的最短路径
gh6 analyze neighbors <user> 查询某用户的直接连接
gh6 analyze degree-dist      各度数的人数分布

gh6 export <file>            导出当前图谱（JSON 格式）
```

- `status` / `run` / `pause` 通过 Unix socket 与守护进程通信
- `analyze` / `export` 直接读取 SQLite，不依赖守护进程
- 所有子命令支持 `--json` 输出 JSON

### 服务管理

```bash
# macOS (launchd)
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.gh6.daemon.plist    # 启动
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.gh6.daemon.plist    # 停止

# Linux (systemd)
systemctl --user start gh6d    # 启动
systemctl --user stop gh6d     # 停止
```

## 通信协议

### 传输层

- Unix Domain Socket，路径：`~/.local/share/gh6/gh6.sock`
- 每行一条 JSON 消息，换行符分隔（JSON Lines）

### 消息格式

```
请求：  {"cmd":"<command>", ...}
响应：  {"type":"ok"|"error"|"event"|"bye", "data":..., "msg":...}
```

### 命令流

```
Client                            Server (gh6d)
  │                                  │
  ├── {"cmd":"status"} ────────────→ │
  │←── {"type":"ok","data":{...}}    │  (立即返回，关闭连接)
  │                                  │
  ├── {"cmd":"status","watch":true}→ │
  │←── {"type":"ok","data":{...}}    │  (先返回当前状态)
  │←── {"type":"event","data":{...}} │  (持续推送)
  │←── {"type":"ok","data":{...}}    │  (定期快照)
  │       (客户端 Ctrl+C 断开)        │  (移除 subscriber)
  │                                  │
  ├── {"cmd":"start"} ─────────────→ │
  │←── {"type":"ok","data":{"msg":"started"}} │
  │                                  │
  ├── {"cmd":"pause"} ─────────────→ │
  │←── {"type":"ok","data":{"msg":"paused"}} │
  │                                  │
  │   (SIGTERM / SIGINT)             │
  │←── {"type":"bye"}                │  (广播退出，守护关闭)
```

- `start` 幂等：已运行时返回 `"already running"`
- `pause` 幂等：已暂停时返回 `"already paused"`
- 守护进程的终止由 launchd / systemd 管理（SIGTERM），不通过 socket 命令

### Status 响应

```json
{
  "type": "ok",
  "data": {
    "users_crawled": 342,
    "users_queued": 1200,
    "current_degree": 3,
    "api_remaining": 4200,
    "api_reset_at": "2026-01-01T12:00:00Z",
    "uptime_secs": 18340,
    "currently_crawling": "alice",
    "paused": false
  }
}
```

### Watch 事件

```json
{"type":"event","data":{"event":"user_done","login":"alice","degree":3,"new_connections":47}}
{"type":"event","data":{"event":"user_queued","login":"bob","degree":4}}
```

### 并发模型

| 场景 | 实现 |
|------|------|
| 多个客户端同时连接 | 每个连接 spawn 独立 tokio task |
| `status`（一次性查询） | 读 `Arc<ServerState>`，返回后关闭 |
| `status --watch` | 订阅 `tokio::sync::broadcast` channel，定期推送快照 |
| `start` / `pause` | 设置 `paused: AtomicBool`，爬虫主循环检查 |
| 多次 start / pause | 幂等，重复请求返回 `"already running"` / `"already paused"` |
| SIGTERM / SIGINT | 信号 handler 设 `shutdown: AtomicBool`，完成当前迭代后优雅退出 |

### 边界情况

| 情况 | 处理 |
|------|------|
| 守护未运行，客户端连接失败 | 提示 `gh6d daemon is not running` |
| 守护已在运行，重复启动 | 连接已有 socket 检测存活，拒绝重复启动 |
| 守护崩溃，socket 残留 | 启动时尝试连接，失败则 `unlink` 清理 |
| 守护启动后默认 IDLE | `paused=true`，等待 `gh6 run` 才开始爬取 |
| 队列空 | 自动设 `paused=true` 回到 IDLE 状态 |
| `pause` 时正在处理 API 调用 | 完成当前迭代后暂停，不丢数据 |
| rate-limit sleep 期间暂停 | sleep 可中断（每 1s 检查 paused/shutdown） |
| SIGTERM 关闭 | 完成当前迭代 + 落库后退出，DB 数据一致 |

## 数据库

### 位置
`~/.local/share/gh6/gh6.db`

### 设计哲学：稳定层 + 扩展层

遵循开闭原则——对扩展开放，对修改封闭。表分为两层：

- **稳定层**（不会改）：`users` 身份注册表 + `edges` 关系边，是图的骨架
- **扩展层**（加功能 = 加表）：`user_profiles`、`edge_history`、未来的 `repos`、`orgs` 等

```
┌─────────────────────────────────────────────┐
│ 稳定层                                       │
│                                             │
│  users     — 身份注册表（login → id）          │
│  edges     — 通用关系边，带生命周期              │
│                                             │
├─────────────────────────────────────────────┤
│ 扩展层                                       │
│                                             │
│  user_profiles   资料快照                      │
│  edge_history    关系变更日志                   │
│  crawl_state     爬取进度                      │
│  (未来) repos    仓库                          │
│  (未来) orgs     组织                          │
│  (未来) snapshots 图快照                       │
│  ...                                         │
└─────────────────────────────────────────────┘
```

### 数据流：两条线互不干扰

GitHub API 有两种返回，走不同的入库路径：

| API | 返回内容 | 写入目标 |
|-----|---------|---------|
| `GET /users/{login}/following` | 摘要列表（只有 login + avatar_url） | `users`（新发现） + `edges`（关系） |
| `GET /users/{login}` | 完整 profile（following 数、name 等） | `user_profiles`（资料） |

摘要 API 绝不碰 `user_profiles`，完整 API 绝不碰 `edges`。物理隔离，从架构上杜绝了摘要数据覆盖 profile 的问题。

### 表结构

```sql
-- ============================================
-- 稳定层
-- ============================================

-- 用户身份（login 仅从摘要 API 新发现时写入，之后不再修改）
CREATE TABLE users (
    id             INTEGER PRIMARY KEY,
    login          TEXT NOT NULL UNIQUE,
    discovered_at  TEXT DEFAULT (datetime('now'))
);

-- 通用关系边（带生命周期）
CREATE TABLE edges (
    from_user_id   INTEGER NOT NULL REFERENCES users(id),
    to_user_id     INTEGER NOT NULL REFERENCES users(id),
    edge_type      TEXT NOT NULL,      -- 'follows', 'starred', 'org_member', ...
    weight         REAL DEFAULT 1.0,
    degree         INTEGER,            -- 从种子用户出发的 BFS 度数
    metadata       TEXT,               -- JSON: {"repo_id":123, "org_id":456, ...}
    is_active      INTEGER NOT NULL DEFAULT 1,  -- 1=当前有效, 0=已失效
    first_seen_at  TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at   TEXT NOT NULL DEFAULT (datetime('now')),
    removed_at     TEXT,               -- NULL = 仍然有效
    PRIMARY KEY (from_user_id, to_user_id, edge_type)
);

-- ============================================
-- 扩展层（第一期）
-- ============================================

-- 用户资料（仅从完整 API 写入，摘要 API 绝不碰）
CREATE TABLE user_profiles (
    user_id        INTEGER PRIMARY KEY REFERENCES users(id),
    name           TEXT,
    avatar_url     TEXT,
    company        TEXT,
    location       TEXT,
    followers      INTEGER NOT NULL,
    following      INTEGER NOT NULL,
    public_repos   INTEGER NOT NULL,
    created_at     TEXT,
    updated_at     TEXT,
    fetched_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 关系变更日志（审计 / 回查）
CREATE TABLE edge_history (
    id             INTEGER PRIMARY KEY,
    from_user_id   INTEGER NOT NULL,
    to_user_id     INTEGER NOT NULL,
    edge_type      TEXT NOT NULL,
    action         TEXT NOT NULL,       -- 'added' | 'removed'
    recorded_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 爬取进度跟踪
CREATE TABLE crawl_state (
    crawler_name   TEXT NOT NULL,
    scope_key      TEXT NOT NULL,
    status         TEXT DEFAULT 'pending',
    priority       TEXT DEFAULT 'normal',
    last_error     TEXT,
    crawled_at     TEXT,
    PRIMARY KEY (crawler_name, scope_key)
);

-- 全局配置（首次启动写入，之后只读）
CREATE TABLE config (
    key            TEXT PRIMARY KEY,
    value          TEXT NOT NULL
);
-- 预置 key: 'seed' — 种子用户 login

-- (未来) 仓库
-- CREATE TABLE repos (
--     id             INTEGER PRIMARY KEY,
--     owner          TEXT NOT NULL,
--     name           TEXT NOT NULL,
--     stars          INTEGER,
--     ...
-- );

-- (未来) 图快照
-- CREATE TABLE snapshots (
--     id             INTEGER PRIMARY KEY,
--     label          TEXT,
--     created_at     TEXT DEFAULT (datetime('now'))
-- );
-- CREATE TABLE snapshot_edges (
--     snapshot_id    INTEGER REFERENCES snapshots(id),
--     from_user_id   INTEGER,
--     to_user_id     INTEGER,
--     edge_type      TEXT,
--     ...
-- );
```

### 边的生命周期

```
[首次发现] ──→ is_active=1 ──→ [再爬时没看到] ──→ is_active=0, removed_at=now
                            ──→ [又看到了] ──→ is_active=1, removed_at=NULL
```

| 状态 | 条件 |
|------|------|
| active（当前有效） | `is_active = 1` |
| removed（已失效） | `is_active = 0` |
| 某时间点有效 | `first_seen_at <= T AND (removed_at IS NULL OR removed_at > T)` |

每次边的状态变化都记录到 `edge_history`，可以回答"谁在什么时候 unfollow 了谁"。

图查询（BFS、最短路径等）只需加 `WHERE is_active = 1`，其他逻辑不变。

### profile 更新策略

资料是快照，需要定期刷新以反映变化。两种方式互补：

- **惰性更新**：爬某用户前检查 `user_profiles.fetched_at`，超过阈值则先调完整 API 刷新
- **主动更新**：后台任务查询过期 profile（`fetched_at < datetime('now', '-7 days')`），批量刷新

`user_profiles` 不存在 = 还没查过，排最高优先级先取。解决了此前 `following=0` 导致排程混乱的问题。

### 设计原则

- 新维度的关系直接插入 `edges`，添加新 `edge_type` 即可，不需改表结构
- `metadata` JSON 字段承载该边类型的专属信息，无需为每种边建专门字段
- 新爬虫实现后直接在 `crawl_state` 登记，复用同一套断点续传机制
- **新功能 = 加表，不动稳定层**：`user_profiles`、`edge_history` 都是外挂的，以后 `repos`、`orgs`、`snapshots` 同理
- 摘要数据写入 `users` + `edges`，profile 数据写入 `user_profiles`，两条线物理隔离

## 爬虫核心

### 爬取策略：分层推进

纯 BFS 的问题：浅层没爬完，深层永久冻结；中枢用户（`following > 5000`）拖慢队列。
因此采用三层混合策略：

| 度 | 策略 | 中枢处理 | 目的 |
|----|------|---------|------|
| 0–1 | 严格 BFS，全部照爬 | 不跳过 | 自己和直接朋友，圈子小，全量不贵 |
| 2 | BFS，中枢延后 | `following > 5000` → `priority = low`，日常只取 `normal`，队列空时才取 `low` | 朋友的朋友，大 V 往后排 |
| 3+ | 随机采样 | 不管中枢，随机抽就完事 | 深层保证推进，不会永远卡死 |

`crawl_state` 新增 `degree` 列，`insert_pending_scope` 时直接写入，省去反查 edges 的开销：

```sql
ALTER TABLE crawl_state ADD COLUMN degree INTEGER;
```

取待办时按三层分流：

```
if 度 0-1 有 pending:
    → ORDER BY degree ASC, priority ASC      # 严格 BFS
elif 度 2 有 pending:
    → ORDER BY priority ASC, degree ASC     # BFS + 中枢延后
else:
    → 度 3+ 随机抽                           # ORDER BY RANDOM() 或伪随机
```

### 第一阶段：FollowCrawler

- 方向：单向，只爬 `following`（用户关注的人）
- 边类型：`"follows"`，weight 1.0
- 种子用户：可配置（`gh6d --seed`，默认自动探测 `gh api /user`），存入 `config` 表
- 速率限制：每次 API 调用后检查 `X-RateLimit-Remaining`，接近 0 时 sleep 到重置窗口

### 爬取流程

```
1. 查询 crawl_state 中 crawler_name='follow_crawler' 且 status='pending' 的 scope（优先 low degree）
2. 取 scope_key（user login）
3. 如果 user_profiles 中无此用户或 fetched_at 过期，调 GET /users/{login} 获取完整 profile
4. 调 GET /users/{login}/following 获取该用户的关注列表（摘要）
5. 将新发现的 login 写入 users 表（如不存在；只写 login，不碰 profile）
6. 将 following 关系写入 edges 表（edge_type='follows', degree=当前度+1, is_active=1）
7. 对于每个新发现用户，在 crawl_state 中创建 pending 记录（degree = 当前度+1）
8. 将该 scope_key 标记为 done
9. 检查 AtomicBool 是否应停止
10. 检查速率限制，sleep 或继续下一个
```

与旧流程的关键差异：`users` 表只存 login，profile 数据独立写入 `user_profiles`，
摘要 API 返回的 `GithubUser` 不再用于 upsert profile 字段。

### 扩展预留

后续可添加的爬虫（实现 `Crawler` trait）：

- **FollowersCrawler**：爬 `followers` 方向，edge_type='followed_by'
- **StarCrawler**：爬用户 star 的仓库，找出同仓库的 stargazer，edge_type='starred_same_repo'
- **OrgCrawler**：爬组织成员关系，edge_type='org_member'
- **RepoContributorCrawler**：爬仓库贡献者共现关系

### Crawler trait（草案）

```rust
#[async_trait]
trait Crawler: Send + Sync {
    fn name(&self) -> &str;
    async fn crawl(&self, scope_key: &str, client: &GithubClient, db: &Db) -> Result<CrawlResult>;
    fn pending_scopes(&self, db: &Db) -> Vec<String>;
}

struct CrawlResult {
    new_users: Vec<User>,
    new_edges: Vec<Edge>,
}
```

## 分析模块

所有分析子命令直读 SQLite，不依赖爬虫进程。

| 子命令 | 实现思路 |
|--------|---------|
| `analyze path <u>` | BFS/双向BFS 在 edges 表上查找从 seed → target 的最短路径 |
| `analyze neighbors <u>` | 查询 edges 中 from/to 该用户的边，汇总展示 |
| `analyze degree-dist` | GROUP BY degree 统计人数分布 |

后续可扩展（预加载图到内存用 petgraph/networkx 算法）：
- 中心性分析（Betweenness, PageRank）
- 社区发现（Louvain）
- 用户画像聚类

## 技术栈

- **语言**：Rust (edition 2024)
- **异步运行时**：tokio
- **数据库**：rusqlite
- **HTTP 客户端**：reqwest
- **序列化**：serde + serde_json
- **GitHub API**：gh CLI token（环境变量 `GITHUB_TOKEN` 或 `gh auth token`）
- **CLI**：clap
- **安装方式**：`cargo install`
