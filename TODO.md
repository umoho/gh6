# TODO

## 本次重构：数据库拆分 (v3)

- [ ] 重写 migration 001_init.sql（新 schema：users, user_profiles, edges[含生命周期], edge_history, crawl_state[含 degree]）
- [ ] 删除 migration 002_priority.sql（已并入 001）
- [ ] 更新 `types.rs`：User 拆分 / GithubUser 拆分
- [ ] 更新 `github.rs`：GhUser 字段改为 Option<i64>，区分 null vs 0
- [ ] 更新 `db.rs`：
  - [ ] upsert_user → insert_user(login) + upsert_profile
  - [ ] 所有读 users 的查询 JOIN user_profiles
  - [ ] pending_scopes 实现三层分流（度 0-1 BFS / 度 2 BFS+中枢延后 / 度 3+ 随机）
  - [ ] crawl_state 加 degree 列，insert_pending_scope 写入 degree
- [ ] 更新 `crawlers/mod.rs`：crawl_following 写 users（只插 login）不碰 profile
- [ ] 更新 `server.rs`：crawl_loop 惰性 fetch profile 逻辑适配新表
- [ ] 更新 `analyze.rs`：所有读 User 的地方适配新结构
- [ ] 更新 `main.rs` 的导出等逻辑

## 基础设施

- [x] 初始化 Rust 项目（Cargo.toml、依赖：tokio, rusqlite, reqwest, serde, serde_json, clap）
- [x] 项目目录结构搭建（src/main.rs, src/db.rs, src/github.rs, src/crawlers/, src/analyze.rs）
- [x] SQLite 数据库初始化（创建 `~/.local/share/gh6/`，migration：users, edges, crawl_state）
- [x] GitHub Token 获取（优先 `GITHUB_TOKEN` 环境变量，fallback `gh auth token`）

## 通信层

- [x] Unix socket 监听模块（`gh6.sock`）
- [x] JSON Lines 协议：解析请求 / 构造响应
- [x] ServerState 共享状态（`Arc<…>` + `AtomicBool`）
- [x] broadcast channel（watch 订阅推送）
- [x] 启动时检测已有实例（拒绝重复启动 / 清理残留 socket）
- [x] 优雅停止（完成当前 API 调用 + 落库后退出）
- [x] `start` / `pause` 命令（控制 `paused: AtomicBool`）
- [x] SIGTERM / SIGINT 信号处理

## 爬虫核心

- [x] Crawler trait 定义
- [x] FollowCrawler 实现
  - [x] 获取用户 following 列表（`GET /users/{login}/following`）
  - [x] 分页处理（Link header）
  - [x] 写入 users 表
  - [x] 写入 edges 表（edge_type='follows'）
  - [x] 更新 crawl_state
- [x] BFS 主循环
  - [x] 从 crawl_state 取 pending scope（优先低度数）
  - [x] 调用爬虫
  - [x] 速率限制处理（检查 X-RateLimit-Remaining，sleep 到重置）
  - [x] 检查停止标志（shutdown）和暂停标志（paused）
  - [x] broadcast 推送事件
- [x] 种子用户初始化（umoho，degree=0）
- [x] 队列空时自动 IDLE
- [x] 守护启动时默认 IDLE（paused=true，等待 gh6 run）

## CLI 命令

- [x] `gh6d` — 守护进程（lib.rs 共享模块，src/bin/gh6d.rs 入口）
- [x] `gh6 run` — 开始 / 恢复爬取
- [x] `gh6 pause` — 暂停爬取
- [x] `gh6 status` — 连接 socket，请求并展示状态
- [x] `gh6 status --watch` — 持续接收事件流
- [x] `gh6 status --watch --progress` — 实时状态栏
- [x] `gh6 analyze path <user>` — BFS 最短路径查询
- [x] `gh6 analyze neighbors <user>` — 直接连接查询
- [x] `gh6 analyze degree-dist` — 度数分布统计
- [x] `gh6 analyze stats` — 数据库概况
- [x] `gh6 analyze export <file>` — 导出图谱 JSON
- [x] 所有命令的 `--json` flag 支持

## 输出格式化

- [x] 人类可读输出（表格、彩色终端）
- [x] JSON 输出模式（`--json` flag）
- [x] Watch 模式事件格式化输出

## 健壮性

- [ ] API 错误重试（429 / 5xx）
- [x] SQLite WAL 模式
- [x] Ctrl+C / SIGTERM 优雅处理（signal handler）
- [ ] 数据库迁移框架（简单版：version table）

## 未来功能（尚未排期）

- [ ] profile 惰性刷新（crawl 前检查 fetched_at 过期则重新 fetch）
- [ ] profile 主动更新（后台任务定时刷新过期 profile）
- [ ] 关系变更检测（对比新 following 列表与现有 edges，记录 unfollow / refollow）
- [ ] StarCrawler、OrgCrawler 等新爬虫
- [ ] 图快照（snapshots 表，支持时间点对比）
- [ ] PageRank / Betweenness 中心性分析
- [ ] 社区发现（Louvain）优化
