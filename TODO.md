# TODO

- [x] 重写 migration 001_init.sql（新 schema）
- [x] 删除 migration 002_priority.sql
- [x] 拆分 types.rs：GithubUser → GithubUserSummary + GithubUserProfile
- [x] 拆分 types.rs：User 适配新表结构
- [x] 更新 github.rs：get_following 返回 Vec<GithubUserSummary>
- [x] 更新 github.rs：get_user 返回 GithubUserProfile
- [x] 新增 db.rs：insert_user / upsert_profile
- [x] 新增 db.rs：insert_edge + edge_history 记录
- [x] 重写 db.rs：pending_scopes 三层分流
- [x] 更新 db.rs：所有读 User 的查询 JOIN user_profiles
- [x] 新增 db.rs：get_config / set_config
- [x] 更新 db.rs：insert_pending_scope 写入 degree
- [x] 更新 crawlers/mod.rs：crawl_following 只写 login 到 users
- [x] 更新 server.rs：seed 逻辑改用 --seed + 自动探测 + config
- [x] 新增 gh6d --seed 参数
- [x] 更新 server.rs：crawl_loop 惰性 fetch profile
- [x] 更新 analyze.rs 适配 Option<i64>
- [x] 更新 main.rs：analyze path --from 从 config 读种子
- [x] 修复 github.rs：手动分页替代 --paginate（中枢用户不卡死）
- [ ] 优化：分页循环内逐页写 DB，避免被杀时丢失已拉取的数据

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
- [x] 种子用户初始化（可配置，degree=0）
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
