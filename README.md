# gh6 — GitHub Social Graph Explorer

> 基于六度分隔理论的 GitHub 社交图谱爬虫与分析工具。  
> 从关注关系出发，探索 GitHub 用户的社交网络，发现连接与桥接点。

[![Crates.io][crates-badge]][crates-url]
[![MIT][mit-badge]][mit-url]

[English](README_EN.md)

[crates-badge]: https://img.shields.io/badge/crates.io-0.1.0-orange
[crates-url]: https://crates.io/crates/gh6
[mit-badge]: https://img.shields.io/badge/license-MIT-blue
[mit-url]: https://opensource.org/licenses/MIT

---

## 概述

`gh6` 是一个 **客户端-守护进程** 架构的工具，以 BFS（广度优先搜索）的方式从种子用户出发，沿着 GitHub 用户的关注（following）关系逐步爬取社交图谱。所有数据存储在本地 SQLite 数据库中，并通过丰富的 CLI 提供分析能力：最短路径查询、共同关注、社区发现、桥接点识别以及用户推荐。

名字源于 *六度分隔理论（six degrees of separation）* —— `gh6` 探索的是 GitHub 上任意两个用户之间到底有多近。

## 架构

```
┌─────────────────────────────────────────────────────────────┐
│              gh6d (守护进程) + gh6 (客户端)                   │
│                                                             │
│  ┌──────────────┐          ┌──────────────────────────────┐ │
│  │    gh6d      │ ──写入──→  ~/.local/share/gh6/gh6.db    │ │
│  │  (launchd /  │          │         SQLite               │ │
│  │   systemd)   │          └──────────────┬───────────────┘ │
│  └──────┬───────┘                         │  直读            │
│         │  Unix socket                    │                 │
│         │  ~/.local/share/gh6/gh6.sock    │                 │
│  ┌──────┴──────────┐          ┌──────────┴───────────────┐ │
│  │ gh6 run         │          │  gh6 analyze {子命令}      │ │
│  │ gh6 pause       │          │  gh6 export               │ │
│  │ gh6 status      │          └──────────────────────────┘ │
│  └─────────────────┘                                       │
└─────────────────────────────────────────────────────────────┘
```

### 组件

| 组件 | 说明 |
|------|------|
| **gh6d** | 后台守护进程，由 launchd（macOS）/ systemd（Linux）管理。启动后空闲等待命令，通过 Unix socket 与客户端通信。 |
| **gh6**  | CLI 客户端，通过 Unix socket 向 gh6d 发送命令（`run`、`pause`、`status`），并执行离线分析。 |

## 快速开始

### 前置条件

- Rust（edition 2024）— 推荐使用 `rustup` 安装
- [GitHub personal access token](https://github.com/settings/tokens)，需包含 `read:user` 权限
  - 通过 `GITHUB_TOKEN` 环境变量设置，或
  - 先运行 `gh auth login`（工具会 fallback 到 `gh auth token`）

### 安装

```bash
# 构建并安装两个二进制文件
cargo install --path .

# 或使用安装脚本（macOS launchd 集成）
./install.sh
```

### 启动守护进程

```bash
# 如果使用 install.sh — launchd 会自动管理
# 否则手动启动：
gh6d --seed <你的GitHub用户名> --workers 3
```

### 开始爬取

```bash
gh6 run                   # 开始/恢复爬取
gh6 status                # 查看进度
gh6 status --watch        # 实时事件流
gh6 status --watch --progress  # 实时状态栏
gh6 pause                 # 暂停爬取（守护进程继续保持运行）
```

## CLI 参考

### 守护进程

```
gh6d [选项]

选项：
  --seed <用户名>    爬取的种子用户（默认使用已认证用户）
  --workers <N>      并行工作线程数（默认：3）
```

### 客户端命令

#### 爬取控制

| 命令 | 说明 |
|------|------|
| `gh6 run` | 开始或恢复爬取 |
| `gh6 pause` | 暂停爬取（守护进程保持运行） |
| `gh6 status` | 查看当前爬取进度快照 |
| `gh6 status --watch` | 实时事件流 |
| `gh6 status --watch --progress` | 实时状态栏（底部） |

#### 图谱分析

| 命令 | 说明 |
|------|------|
| `gh6 analyze route <用户名> [--from <用户名>] [--limit N] [--fuzzy]` | 查询两用户间的最短路径 |
| `gh6 analyze common <用户名> <用户名> [--limit N]` | 查询共同关注与共同粉丝 |
| `gh6 analyze user <用户名> [--detail]` | 查看用户档案与社交关系 |
| `gh6 analyze suggest <用户名> [--limit N]` | 基于共同关注的用户推荐 |
| `gh6 analyze bridges [--limit N]` | 发现连接不同社区的桥接节点 |
| `gh6 analyze communities [--limit N] [--user <用户名>]` | 社区发现 |
| `gh6 analyze stats` | 数据库概览 |
| `gh6 export <文件路径>` | 将图谱导出为 JSON |

所有分析命令均支持 `--json` 参数，输出机器可读的 JSON 格式。

## 功能详解

### 爬虫

- **BFS 遍历 GitHub 关注图** — 从种子用户出发，逐层沿 following 边向外扩展
- **中枢用户延迟爬取** — 关注数超过 5000 的用户会被延后处理，避免耗尽 API 速率限制
- **并行工作线程** — 可配置的并发度（默认 3），支持原子化领取爬取范围
- **可中断恢复** — 爬取状态持久化在 SQLite 中，随时暂停和恢复
- **优雅关闭** — 在翻页中途也能响应 `SIGINT`/`SIGTERM`
- **速率限制感知** — 自动适应 GitHub API 的速率限制

### 分析

- **最短路径** — 基于 BFS 的路径搜索，支持模糊用户名匹配
- **共同连接** — 两用户之间的关注/粉丝交集
- **用户推荐** — 基于共同关注的协同过滤
- **桥接点检测** — 识别连接不同社区的关键用户
- **社区发现** — 社交图中的群体划分

### 数据库

- **SQLite** — 本地存储，路径为 `~/.local/share/gh6/gh6.db`
- **分层 Schema** — 稳定层（用户、关注关系）+ 扩展层（档案、历史、爬取状态）
- **关系历史** — 记录关注关系的变化，支持时间维度分析
- **配置表** — 键值对配置与爬取数据一同持久化

## 服务管理

### macOS（launchd）

```bash
# 安装（构建二进制并加载 launchd plist）
./install.sh

# 卸载
./install.sh uninstall

# 手动控制
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.gh6.daemon.plist
launchctl bootout  gui/$UID ~/Library/LaunchAgents/com.gh6.daemon.plist
```

### Linux（systemd — 用户级）

```bash
# 复制 service 文件
cp gh6d.service ~/.config/systemd/user/

# 启用并启动
systemctl --user daemon-reload
systemctl --user enable --now gh6d

# 查看日志
journalctl --user -u gh6d -f
```
## 项目结构

```
src/
├── main.rs         # gh6 CLI 入口（clap 子命令）
├── lib.rs          # 模块声明
├── types.rs        # 共享类型定义
├── db.rs           # SQLite 数据库层
├── github.rs       # GitHub API 客户端
├── server.rs       # 守护进程（socket 监听、爬取循环）
├── display.rs      # 终端输出原语与视图
├── analyze.rs      # 图谱分析查询
├── crawlers/
│   └── mod.rs      # BFS 爬取逻辑（FollowCrawler）
└── bin/
    └── gh6d.rs     # 守护进程入口

migrations/
└── 001_init.sql    # 数据库 schema
```

## 相关文档

- [DESIGN.md](DESIGN.md) — 详细架构与设计决策
- [TODO.md](TODO.md) — 开发进度与路线图
- [AGENTS.md](AGENTS.md) — AI 辅助开发的项目约定

## 许可证

MIT
