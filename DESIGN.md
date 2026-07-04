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

### 表结构

```sql
-- 用户基础信息
CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    login         TEXT NOT NULL UNIQUE,
    name          TEXT,
    avatar_url    TEXT,
    company       TEXT,
    location      TEXT,
    followers     INTEGER,
    following     INTEGER,
    public_repos  INTEGER,
    created_at    TEXT,
    updated_at    TEXT
);

-- 通用多类型关系边
CREATE TABLE edges (
    from_user_id   INTEGER NOT NULL REFERENCES users(id),
    to_user_id     INTEGER NOT NULL REFERENCES users(id),
    edge_type      TEXT NOT NULL,      -- 'follows', 'followed_by', 'starred_same_repo', ...
    weight         REAL DEFAULT 1.0,
    degree         INTEGER,            -- 从种子用户出发的 BFS 度数
    metadata       TEXT,               -- JSON: {"repo_id":123, "org_id":456, ...}
    discovered_at  TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (from_user_id, to_user_id, edge_type)
);

-- 爬取进度跟踪（每个爬虫独立追踪）
CREATE TABLE crawl_state (
    crawler_name   TEXT NOT NULL,      -- 'follow_crawler', 'star_crawler', ...
    scope_key      TEXT NOT NULL,      -- 爬取单元标识（user_id, repo_id, ...）
    status         TEXT DEFAULT 'pending',  -- pending / done / error
    last_error     TEXT,
    crawled_at     TEXT,
    PRIMARY KEY (crawler_name, scope_key)
);
```

### 设计原则

- 新维度的关系直接插入 `edges`，添加新 `edge_type` 即可，不需改表结构
- `metadata` JSON 字段承载该边类型的专属信息，无需为每种边建专门字段
- 新爬虫实现后直接在 `crawl_state` 登记，复用同一套断点续传机制

## 爬虫核心

### 第一阶段：FollowCrawler

- 方向：单向，只爬 `following`（用户关注的人）
- 边类型：`"follows"`，weight 1.0
- 种子用户：`umoho`
- 策略：BFS 逐层全量扩展
- 速率限制：每次 API 调用后检查 `X-RateLimit-Remaining`，接近 0 时 sleep 到重置窗口

### BFS 流程

```
1. 查询 crawl_state 中 crawler_name='follow_crawler' 且 status='pending' 的最小 degree
2. 取 scope_key（user login），调用 GitHub API 获取该用户的 following 列表
3. 将新发现的用户写入 users 表（如不存在）
4. 将 following 关系写入 edges 表（edge_type='follows', degree=当前度+1）
5. 对于每个新发现用户，在 crawl_state 中创建 pending 记录（degree+1）
6. 将该 scope_key 标记为 done
7. 检查 AtomicBool 是否应停止
8. 检查速率限制，sleep 或继续下一个
```

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
