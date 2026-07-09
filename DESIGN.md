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

### 源码目录结构

```
src/
├── lib.rs                     # 库根：pub mod db; pub mod types;
├── types.rs                   # 共享类型合约（GitHub API / DB / Server / Crawl 类型）
├── db/
│   ├── mod.rs                 # 数据库层（稳定层 + 扩展层 + 分析查询）
│   └── migrations/            # SQL 迁移脚本
│       ├── 001_init.sql
│       └── 002_defer_hub_scopes.sql
└── bin/
    ├── gh6/                   # gh6 客户端 target
    │   ├── main.rs            # CLI 入口：clap 解析、socket 通信、结果输出
    │   ├── analyze.rs         # 分析模块（route, common, user, suggest, bridges, communities, stats, export）
    │   ├── display.rs         # 终端显示（tree, header, grid, bar 原语 + Display impl）
    │   └── tui.rs             # TUI 全屏监控（ratatui + crossterm）
    └── gh6d/                  # gh6d 守护进程 target
        ├── main.rs            # 守护入口：clap 解析、env_logger 初始化
        ├── server.rs          # Unix socket 服务 + crawl_loop + 状态管理
        ├── github.rs          # GitHub API 客户端（通过 gh CLI）
        └── crawlers/
            └── mod.rs         # Crawler trait + FollowCrawler 实现
```

- **`src/lib.rs`** 仅声明两个公共模块（`db` / `types`）和一个常量。分析、显示、爬虫逻辑分别属于 gh6/gh6d 两个 target，编译器在构建单个 binary 时不会解析对方的源码文件
- **`src/bin/gh6/main.rs`** 和 **`src/bin/gh6d/main.rs`** 是 Cargo 目录式 binary 约定，各自是独立的 crate root，通过 `mod` 声明所属的私有模块，通过 `use gh6::...` 引用 lib 中的公共类型和数据库方法
- **迁移文件**与数据库代码放在一起（`src/db/migrations/`），不再散落项目根目录

## 命令体系

```
gh6d                             启动守护进程（由 launchd / systemd 管理，无需手动调用）
gh6 run                          开始 / 恢复爬取
gh6 pause                        暂停爬取（守护进程保持运行）
gh6 status                        查看进度（一次性查询）
gh6 status tui                    实时监控（TUI 全屏界面）

gh6 analyze route <LOGIN>        查询从种子用户到目标用户的最短路径
             [--from <LOGIN>]    指定起点（默认读取 config seed）
             [--limit <N>]       显示前 N 条路径（默认 1，0 = 全部）
             [--fuzzy]           模糊搜索模式

gh6 analyze common <LOGIN> <LOGIN>  查询两用户共同关注和共同粉丝
               [--limit <N>]        结果上限

gh6 analyze user <LOGIN>            用户档案与社交关系
             [--detail]             显示完整列表（默认截断 10 人）

gh6 analyze suggest <LOGIN>         基于共同关注的用户推荐
               [--limit <N>]        推荐数量（默认 20）

gh6 analyze bridges                 发现桥梁节点
               [--limit <N>]        结果上限（默认 20）

gh6 analyze communities             社区发现（Louvain 算法）
               [--limit <N>]        显示社区数（默认 10）
               [--user <LOGIN>]     查询某用户所在社区

gh6 analyze stats                   数据库与图统计概览

gh6 analyze export <FILE>           导出图到 JSON 文件
```

- `status` / `run` / `pause` 通过 Unix socket 与守护进程通信
- `analyze` / `export` 直接读取 SQLite，不依赖守护进程
- 所有子命令（除 `status tui` 外）支持 `--json` 输出 JSON
- `status tui` 为全屏交互界面，不支持 `--json`

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

### 爬取策略：统一调度

纯 BFS 的问题：浅层没爬完，深层永久冻结；大 V 用户（`following > 5000`）拖慢队列。
原来的三层混合策略（degree ≤ 1 不跳过大 V、degree 2 中枢延后、degree ≥ 3 随机采样）
在遇到直连大 V 时仍会产生大量 scope 塞满队列。

因此改为统一策略：**不再按 degree 分层，所有 scope 同一套规则。**

| 用户特征 | 处理方式 | 新 scope 的 priority |
|----------|---------|---------------------|
| 普通用户（following ≤ 5000） | 正常爬取 | `normal` |
| 大 V（following > 5000） | 正常爬取（保证边数据完整） | `low`（继承自父 scope） |

#### 调度排序

所有 pending scope 按统一规则排序，不再分流：

```
ORDER BY priority ASC,   -- high → normal → low
         degree ASC,     -- 同优先级内浅的优先
         error_count ASC -- 同度内出错少的优先
```

不再需要 RANDOM() 随机采样——大 V 的关注者设 low priority 后队列自然可控，
不会出现 single source 撑爆队列的情况。

#### 大 V 判定时机

大 V 判定在 **profile 查询阶段**（每次爬取前调用 `GET /users/{login}`）进行：

```
following_count ≥ HUB_FOLLOWING_THRESHOLD（5000）
  └── 是 → 将本 scope 的 priority 设为 low（下次不再优先爬取本人）
          但仍正常执行 crawl_following，新 scope 也继承 low priority
  └── 否 → 保持 normal，正常执行 crawl_following
```

#### priority 继承规则

新 scope 入队时的 priority 取决于**当前被爬用户**：

```
insert_pending_scope(scope_key, degree, priority = current_user_is_hub ? "low" : "normal")
```

- 大 V 发现的所有用户 → `low`（不堵队列）
- 普通人发现的所有用户 → `normal`（正常调度）
- 已在队列中的 scope 不会被覆盖（INSERT OR IGNORE，先到先得）

### 第一阶段：FollowCrawler

- 方向：单向，只爬 `following`（用户关注的人）
- 边类型：`"follows"`，weight 1.0
- 种子用户：可配置（`gh6d --seed`，默认自动探测 `gh api /user`），存入 `config` 表
- 速率限制：每次 API 调用后检查 `X-RateLimit-Remaining`，接近 0 时 sleep 到重置窗口

### 爬取流程

每条 scope 的处理流程：

```
1. claim_scope：认领一条 pending scope（按 priority ASC, degree ASC, error_count ASC 排序）
2. 取 scope_key（user login）
3. 调 GET /users/{login} 获取完整 profile（无论是否已缓存）
    ├── 将 profile 写入 user_profiles（INSERT OR REPLACE）
    └── 拿到 following_count：
          ├── ≥ 5000 → 将本 scope 的 priority 设为 low
          └── < 5000 → 保持 normal
4. 调 GET /users/{login}/following 获取该用户的关注列表（摘要）
    ├── 将新发现的 login 写入 users 表（如不存在；只写 login，不碰 profile）
    ├── 将 following 关系写入 edges 表（edge_type='follows', degree=当前度+1, is_active=1）
    └── 对于每个新发现用户：
          ├── 如果尚未在 crawl_state 中：创建 pending 记录
          │     └── priority = 如果当前用户是 ≥ 5000 的大 V 则 low，否则 normal
          └── 如果已有 crawl_state 记录：忽略（INSERT OR IGNORE）
5. 将该 scope_key 标记为 done
6. 广播 CrawlEvent（UserDone + UserQueued）
7. 检查 AtomicBool 是否应停止
8. 检查速率限制，sleep 或继续下一个
```

关键差异：
- **profile 查询不再有条件**——每次爬取前都调完整 API（不再是"如果 user_profiles 中无此用户或 fetched_at 过期"）。
  profile 数据本身用于更新 `user_profiles`，其中 `following` 字段用于大 V 判定。
- **crawl_following 不再按 degree 分层**——所有用户同一套规则，大 V 不跳过，但其新 scope 继承 low priority。

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
|--------|--------—|
| `route` | BFS 最短路径 + DFS 全路径搜索，支持模糊匹配 |
| `common` | SQL JOIN 查找共同关注 / 共同粉丝 |
| `user` | LEFT JOIN user_profiles + 边表查找关注/粉丝/互关 |
| `suggest` | Adamic-Adar 加权推荐算法 |
| `bridges` | 模拟移除每个节点，检测连通分量变化 |
| `communities` | Louvain 社区检测算法，含模块度 Q 值 |
| `stats` | 聚合查询：用户数、边数、度数分布、图密度、连通分量 |
| `export` | 全量导出 users + edges 到 JSON |

## 显示架构 (Display Kit)

### 设计原则

- **数据与视图分离**：`analyze.rs` 生产类型化结果，`display/` 模块负责渲染
- **`impl Display` trait**：每个输出类型实现 `fmt::Display`，`main.rs` 只需 `println!("{}", data)`
- **显示变体 = newtype wrapper**：如 `UserView { data, detail }`，不同 flag 组合对应不同 newtype
- **部件化组合**：所有子命令由有限的绘制原语拼接，杜绝各画各的

### 绘制原语

所有树形布局统一为一个 `tree` 原语。`card`、`tree_block`、`tree_grid`、`nested_tree` 都是它的特例。

#### 核心原语

```rust
pub struct TreeNode {
    /// This line's content (pre-formatted by caller).
    pub content: String,
    /// Child nodes (empty = leaf).
    pub children: Vec<TreeNode>,
}

/// Render a tree with `├` / `└` / `│` prefixes.
///
/// ```text
///   root                          ← level 0, no prefix
///   ├ item1                       ← level 1
///   │ ├ item1.child1              ← level 2
///   │ └ item1.child2
///   └ item2                       ← level 1
/// ```
pub fn tree(root: &str, items: &[TreeNode]) -> String;
```

| 原语 | 说明 | 内部用 tree? |
|------|------|------------|
| `tree(root, items)` | 统一树形 — 所有缩进来源于此 | — |
| `header(emoji, title, meta)` | 统一头部，如 `🗺️ A 到 B 共 100 条路径` | ✗ |
| `align_grid(headers, rows)` | 无边框列对齐（bridges）。宽度计算剥 ANSI | ✗ |
| `footer(text)` | dim 脚注 | ✗ |

#### 行内级

| 原语 | 说明 |
|------|------|
| `path_chain(users)` | `alice · bob · eve` — 起点 bold，终点 green+bold，`·` dim |
| `directed_edge(from, to)` | `alice dim(→) bob` |
| `num(n)` | `301,434`（千分位） |
| `bar(value, max, width)` | `████`（monochrome） |
| `weight_bar(value, max, width)` | `████`（green→yellow→red 渐变） |
| `visible_width(s)` | 剥 ANSI escape 后计算 Unicode 显示宽度 |

#### 样式 token

**三层色彩体系：**

| 层级 | 用途 | token |
|------|------|-------|
| L0 本体 | 数字、中文正文、普通 login | regular（无色） |
| L1 强调 | 用户 login、路径终点、阈值告警 | `blue`, `green`+`bold`, `red`/`yellow`/`green` |
| L2 辅助 | 标签、解释、占位符 | `dim` |

| token | 说明 |
|-------|------|
| `dim(s)` | 灰底辅助文本 |
| `bold(s)` | 粗体强调 |
| `blue(s)` / `green(s)` / `yellow(s)` / `red(s)` / `cyan(s)` | 语义着色 |
| `suffix(s)` | 解释性后缀，等价 `dim(s)`（如 `为代表`、`也关注了 ta`） |
| `density_color(d)` / `impact_color(n)` / `modularity_color(q)` | 阈值着色 |

### 子命令渲染组成

#### route

```text
🗺️ bold(from) 到 green+bold(query)  dim(共 N 条路径)

  bold(start) · mid · green+bold(end)  dim(N 步)
  ├ from dim(→) to
  └ ...
```

- header：from bold，target green+bold，meta dim
- 每条路径：`tree(path_chain + dim(步数), [edges...])` — edges 没有 children
- fuzzy：`tree(blue(matched_login), [TreeNode { content: path_chain, children: edges }])`

#### common

```text
👥 blue(A) 和 blue(B)

  dim(共同关注) 3 人
  ├ user1
  └ user2

  dim(共同粉丝) 0 人
  └ dim(无)
```

- 标签 `共同关注`/`共同粉丝` 用 dim，数字 regular

#### user

```text
👤 blue(login)

  基本信息
  ├ 姓名      value
  ├ 公司      dim(—)
  └ 账号创建  2020-01-15

  统计
  ├ 关注      12 人  dim(已获取 12 人)
  ├ 粉丝       8 人  dim(已获取  3 人)    ← 数字右对齐
  └ 公开仓库  41 个

  社交关系
  ├ bold(green(→) 关注) dim(10 人)
  │ ├ user1
  │ └ ...
  ├ bold(yellow(⇄) 互关) dim(2 人)
  │ └ ...
  └ bold(cyan(←) 粉丝) dim(1 人)
      └ ...
```

- 统计列内部右对齐（`{:>N}` pad），`已获取` 竖向对齐
- 社交标签 bold + 箭头保留颜色，数字 dim
- `profile_crawled` 为 false 时所有 value 显示 `dim(—)`

#### suggest

```text
💡 基于 blue(A) 的社交圈推荐  dim(top N)

  blue(login)  weight_bar  weight
  └ suffix(等 N 人也关注了 ta)

基于 dim(N) 个关注者，覆盖 dim(N) 个候选
```

- header `top N` dim
- friends 行整体 dim（用 suffix）
- footer 只有数字 dim，中文正文 regular

#### bridges

```text
🌉 桥梁节点  dim(top N)
隐藏后连通分量从 N 增加

  dim(#)  bold(login)  bold(关注)  bold(粉丝)  bold(关键性)
  dim(#1) blue(login1)  N  N  red(+N)
  dim(#2) blue(login2)  ...
```

- 列标题行 bold
- 行序号 dim
- impact 阈值着色
- `visible_width` 确保列宽计算不受 ANSI 影响

#### communities

```text
🏘️ 社区发现  dim(共 N 个社区)

  Louvain 算法  dim(模块度)  Q = green(N)

  bold(ID)  N 人
  └ alice, bob, eve suffix(为代表)

仅显示前 dim(N) 个社区
```

- 社区 ID bold，不编号
- Q 值阈值着色
- `为代表` 用 suffix

#### stats

```text
📊 gh6 数据库

  数据库概况
  ├ 用户总数  301,434
  ├ ...

  度数分布
    3°   4,967  ██████████

  图统计
  ├ 边数          408,501
  ├ 图密度        0.000004   ← 阈值着色
  └ ...
```

- KV 对放在 `tree` 内，标题 bold
- 度数分布独立（bar 图表不适合 tree）

#### status（一次性查询）

```text
⏳ gh6

  状态
  ├ 服务状态    ▶ 运行中
  ├ 已爬        5,623
  ├ ...
  └ 运行时间    2h 3m 5s
```

- 去掉 header 上的 uptime（下面已显示）
- KV 对用 `tree` 收拢
- 实时监控场景使用 `gh6 status tui`（见下方 TUI 章节）

## TUI — gh6 status tui

`gh6 status tui` 使用 ratatui + crossterm 提供全屏交互式实时监控界面，
替代旧版 `gh6 status --watch --progress` 的手工 ANSI 方案。

### 设计原则

- **UI 库**：ratatui + crossterm，接管终端全屏（alternate screen + raw mode）
- **只负责 `status tui`**：analyze 等其他命令继续走 Display Kit，互不干扰
- **数据类型不变**：读取的 `StatusData` / `CrawlEvent` 与 daemon 端协议一致
- **事件缓冲**：`VecDeque<String>` 环形缓冲，上限 9999 条，超出自动丢弃

### 布局

```
┌──────────────────────────────────────────────┐
│  [1°] alice                        done  +5  │  ← 事件日志区
│  [2°] bob                         queued     │     可滚动查看
│  [3°] some-very-long-login        done  +12  │     最多缓存 9999 条
│  ...                                         │
├──────────────────────────────────────────────┤
│  crawling  alice (1°)  bob (2°)  ...         │  ← 活跃 worker 行
│  crawled 1,234  queue 56  retry 3  API 4,567 │  ← 统计 + API 状态行
└──────────────────────────────────────────────┘
```

- **上区**：事件日志，login 居左、状态（done/queued）居右，`ratatui::widgets::Paragraph`
- **下区**：固定两行状态栏。第一行列出所有活跃 worker，第二行全局统计 + API 状态
- API 剩余按阈值着色：≥1000 绿、≥100 黄、<100 红
- 暂停时状态栏显示不同内容（⏸ 队列 N，等待 `gh6 run`）

### 事件格式

```text
[1°] alice                                    done  +5 connections
[2°] bob-two-million-very-long-login          queued
```

- `[N°]` 度数用 cyan，login 用 blue
- 右侧状态：`done` 绿 + 连接数，`queued` dim
- 左右对齐用 ratatui `Line` + `Alignment::SpaceBetween`

### 键盘快捷键

| 键 | 功能 |
|----|------|
| `q` / `Esc` / `Ctrl+C` | 退出 TUI，断开 socket，清理终端 |
| `↑` / `↓` / `j` / `k` | 滚动事件日志 |
| `PgUp` / `PgDn` | 翻页 |
| `g` / `G` | 跳到顶 / 跳到底 |
| `r` | 强制重绘（终端 resize 时自动处理，一般不需要） |

### 数据流

```
gh6d (daemon)                     gh6 status tui
    │                                    │
    ├── {"type":"ok","data":{...}} ────→ │  初始状态快照
    ├── {"type":"event","data":{...}} ──→ │  UserDone / UserQueued
    ├── {"type":"ok","data":{...}} ────→ │  定期状态快照（更新状态栏）
    │       ...                           │
    │       (客户端按 q / Ctrl+C)         │  断开 socket，退出 TUI
```

- socket 读取走 tokio::sync::mpsc channel，推入主循环
- 主循环 `tokio::select!` 同时等 channel、终端输入（crossterm EventStream）、定时重绘（250ms tick）

### 与 --json 的关系

`gh6 status tui --json` 在 clap 层面报错（`--json` 标记为与 `tui` 子命令冲突）。
TUI 是全屏交互模式，JSON 输出无意义。

### 调试

TUI 接管终端后 `println!` 输出不可见。调试方式：

| 方式 | 说明 |
|------|------|
| 日志文件 | `env_logger` 写入 `/tmp/gh6-tui.log`，另一终端 `tail -f` |
| 独立 tmux socket | `tmux -L gh6-tui` 创建隔离 session，`capture-pane -p` 抓取内容 |
| ratatui TestBackend | 单元测试中渲染到内存 buffer 验证布局 |

### 样式约定

- **不使用括号**：`未爬取` (dim)，`—` 表示 `未填写` (dim)
- **统计数字内联标签**：`关注 10 人` 而非 `关注 (10)`
- **所有解释性后缀**：`为代表`、`也关注了 ta`、`已获取 N 人`、`等 N 人` → 统一用 `suffix()`
- **不依赖 tabled crate**：所有显示自行绘制，零外部显示依赖
- **`--json` 输出不受影响**：serde 序列化原数据，不走 display 层

## 技术栈

- **语言**：Rust (edition 2024)
- **异步运行时**：tokio
- **数据库**：rusqlite
- **HTTP 客户端**：reqwest
- **序列化**：serde + serde_json
- **GitHub API**：gh CLI token（环境变量 `GITHUB_TOKEN` 或 `gh auth token`）
- **CLI**：clap
- **TUI**：ratatui + crossterm（用于 `gh6 status tui`）
- **安装方式**：`cargo install`
