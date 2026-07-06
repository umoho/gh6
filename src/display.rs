//! Terminal display primitives and views for gh6 analyze output.
//!
//! # Design
//!
//! All output flows through a small set of composable drawing primitives.
//! Each analyze result type implements [`fmt::Display`] by calling these
//! primitives — no ad‑hoc `println!()` anywhere.
//!
//! # Primitives
//!
//! | Primitive       | Purpose                           |
//! |-----------------|-----------------------------------|
//! | `header`        | `🗺️ A 到 B  共 5 条路径`            |
//! | `section`       | `基本信息`                         |
//! | `tree_block`    | Single‑level key‑value tree        |
//! | `nested_tree`   | Multi‑level tree (user social)     |
//! | `card`          | Head + tree‑prefixed body lines    |
//! | `align_grid`    | Borderless column‑aligned grid     |
//! | `bar`           | Monochrome bar `████`              |
//! | `weight_bar`    | Gradient bar (green→yellow→red)    |
//! | `path_chain`    | `A · B · C`                        |
//! | `spacer`        | Blank line                         |
//! | `footer`        | Dimmed footnote                    |

use std::fmt;

use owo_colors::OwoColorize;
use unicode_width::UnicodeWidthStr;

use crate::analyze::{
    BridgesResult, CommonResult, CommunitiesResult, PathInfo, RouteResult, StatsResult,
    SuggestResult, UserProfileResult,
};
use crate::types::StatusData;

// ── Style tokens ──────────────────────────────────────────────────────────

/// Dim / secondary text.
pub fn dim(s: &str) -> String {
    s.dimmed().to_string()
}

/// Bold emphasis.
pub fn bold(s: &str) -> String {
    s.bold().to_string()
}

/// User‑login blue.
pub fn blue(s: &str) -> String {
    s.blue().to_string()
}

/// Positive / good.
pub fn green(s: &str) -> String {
    s.green().to_string()
}

/// Warning / medium.
pub fn yellow(s: &str) -> String {
    s.yellow().to_string()
}

/// Danger / high.
pub fn red(s: &str) -> String {
    s.red().to_string()
}

/// Degree / accent.
pub fn cyan(s: &str) -> String {
    s.cyan().to_string()
}

/// End‑of‑path user (green + bold).
pub fn target(s: &str) -> String {
    s.green().bold().to_string()
}

// ── Numeric formatting ────────────────────────────────────────────────────

/// Format a u64 with thousands separators.
pub fn num(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ── Threshold colour helpers ──────────────────────────────────────────────

/// Colour a graph density value.
pub fn density_color(d: f64) -> String {
    let s = format!("{d:.6}");
    if d > 0.001 {
        s.green().to_string()
    } else if d > 0.0001 {
        s.yellow().to_string()
    } else {
        s.red().to_string()
    }
}

/// Colour a bridge‑impact value.
pub fn impact_color(impact: usize) -> String {
    let s = format!("+{impact}");
    if impact >= 1000 {
        s.red().to_string()
    } else if impact >= 100 {
        s.yellow().to_string()
    } else {
        s
    }
}

/// Colour a Louvain modularity score.
pub fn modularity_color(q: f64) -> String {
    let s = format!("{q:.4}");
    if q > 0.5 {
        s.green().to_string()
    } else if q > 0.3 {
        s.yellow().to_string()
    } else {
        s.dimmed().to_string()
    }
}

// ── Layout primitives ─────────────────────────────────────────────────────

/// Unified header: `{emoji} {title}  {meta}`.
///
/// `meta` is always dimmed.  Pass an empty string to omit.
pub fn header(emoji: &str, title: &str, meta: &str) -> String {
    if meta.is_empty() {
        format!("{emoji} {title}")
    } else {
        format!("{emoji} {title}  {}", dim(meta))
    }
}

/// Bold section title, e.g. `基本信息`.
pub fn section(title: &str) -> String {
    format!("  {}", bold(title))
}

/// Single blank line.
pub fn spacer() -> &'static str {
    "\n\n"
}

/// Dimmed footnote line.
pub fn footer(text: &str) -> String {
    format!("\n\n{}", dim(text))
}

// ── Tree primitives ───────────────────────────────────────────────────────

/// A single key‑value row inside [`tree_block`].
pub struct TreeItem {
    pub key: String,
    pub value: String,
}

impl TreeItem {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Render a single‑level tree block:
///
/// ```text
///   section title
///   ├ key1    val1
///   ├ key2    val2
///   └ key3    val3
/// ```
pub fn tree_block(title: &str, items: &[TreeItem]) -> String {
    let mut out = section(title).to_string();

    let max_key_w = items
        .iter()
        .map(|it| UnicodeWidthStr::width(it.key.as_str()))
        .max()
        .unwrap_or(0);

    for (i, item) in items.iter().enumerate() {
        out.push('\n');
        let prefix = if i == items.len() - 1 { "└ " } else { "├ " };
        let key_w = UnicodeWidthStr::width(item.key.as_str());
        let pad = max_key_w.saturating_sub(key_w);
        out.push_str(&format!(
            "  {prefix}{}{}  {}",
            item.key,
            " ".repeat(pad),
            item.value
        ));
    }
    out
}

/// A group of items inside [`nested_tree`].
pub struct TreeGroup {
    /// e.g. `→ 关注 10 人`
    pub label: String,
    /// Child items below the label.
    pub items: Vec<String>,
}

/// Render a multi‑level tree (used for user social section):
///
/// ```text
///   section title
///   ├ → group 1
///   │   ├ item a
///   │   └ item b
///   ├ ⇄ group 2
///   │   └ item c
///   └ ← group 3
///       └ item d
/// ```
pub fn nested_tree(title: &str, groups: &[TreeGroup]) -> String {
    let mut out = section(title).to_string();

    for (gi, group) in groups.iter().enumerate() {
        out.push('\n');
        let group_last = gi == groups.len() - 1;
        let g_prefix = if group_last { "└ " } else { "├ " };
        let g_cont = if group_last { "  " } else { "│ " };

        out.push_str(&format!("  {g_prefix}{}", group.label));

        let last_idx = group.items.len().saturating_sub(1);
        for (ii, item) in group.items.iter().enumerate() {
            out.push('\n');
            let item_last = ii == last_idx;
            let i_prefix = if item_last { "└ " } else { "├ " };
            out.push_str(&format!("  {g_cont}{i_prefix}{item}"));
        }
    }
    out
}

/// Render a card: a head line followed by tree‑prefixed body lines.
///
/// ```text
///   head
///   ├ body 1
///   └ body 2
/// ```
pub fn card(head: &str, body: &[String]) -> String {
    let mut s = format!("  {head}");
    if body.is_empty() {
        return s;
    }
    for (i, line) in body.iter().enumerate() {
        s.push('\n');
        let prefix = if i == body.len() - 1 { "└ " } else { "├ " };
        s.push_str(&format!("  {prefix}{line}"));
    }
    s
}

// ── Grid primitives ───────────────────────────────────────────────────────

/// Borderless column‑aligned grid.  The first row is dimmed (header).
pub fn align_grid(headers: &[&str], rows: &[Vec<String>]) -> String {
    assert!(
        !headers.is_empty(),
        "align_grid requires at least one header"
    );
    let ncols = headers.len();

    // Compute column widths.
    let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }

    let mut s = String::new();

    // Header row.
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            s.push_str("  ");
        }
        let w = UnicodeWidthStr::width(*h);
        s.push_str(&dim(h));
        if i < ncols - 1 {
            s.push_str(&" ".repeat(widths[i].saturating_sub(w)));
        }
    }

    // Data rows.
    for row in rows {
        s.push('\n');
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            let w = UnicodeWidthStr::width(cell.as_str());
            s.push_str(cell);
            if i < ncols - 1 {
                s.push_str(&" ".repeat(widths[i].saturating_sub(w)));
            }
        }
    }

    s
}

// ── Bar charts ────────────────────────────────────────────────────────────

/// Monochrome bar `█` repeated proportionally.
pub fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let n = ((value as f64 / max as f64) * width as f64) as usize;
    dim(&"█".repeat(n.max(1)))
}

/// Gradient weight bar: green → yellow → red.
pub fn weight_bar(value: f64, max: f64, width: usize) -> String {
    if max <= 0.0 || width == 0 {
        return String::new();
    }
    let n = ((value / max) * width as f64) as usize;
    let n = n.min(width);
    if n == 0 {
        return String::new();
    }
    let mut s = String::new();
    for i in 0..n {
        let ratio = i as f32 / width.max(1) as f32;
        let ch = if ratio < 0.33 {
            "█".green().to_string()
        } else if ratio < 0.66 {
            "█".yellow().to_string()
        } else {
            "█".red().to_string()
        };
        s.push_str(&ch);
    }
    s
}

// ── Path helpers ──────────────────────────────────────────────────────────

/// Build a path chain like `A · B · C` with bold start and green target.
pub fn path_chain(users: &[&str]) -> String {
    let sep = format!(" {} ", dim("·"));
    let mut s = String::new();
    for (i, u) in users.iter().enumerate() {
        if i > 0 {
            s.push_str(&sep);
        }
        if i == 0 {
            s.push_str(&bold(u));
        } else if i == users.len() - 1 {
            s.push_str(&target(u));
        } else {
            s.push_str(u);
        }
    }
    s
}

/// Render a directed follows edge `from → to`.
pub fn directed_edge(from: &str, to: &str) -> String {
    let arrow = dim("→");
    format!("{from} {arrow} {to}")
}

// ── List truncation ───────────────────────────────────────────────────────

/// Join names, show `等 N 人` if truncated.
pub fn truncate_list(items: &[String], max: usize) -> String {
    if items.is_empty() {
        return dim("无");
    }
    if items.len() <= max {
        return items.join(", ");
    }
    let shown: Vec<&str> = items.iter().take(max).map(|s| s.as_str()).collect();
    let remaining = items.len() - max;
    format!("{} 等 {} 人", shown.join(", "), remaining)
}

// ── Uptime helper ─────────────────────────────────────────────────────────

pub fn fmt_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  View implementations (Display trait)
// ═══════════════════════════════════════════════════════════════════════════

// ── RouteResult ────────────────────────────────────────────────────────────

impl fmt::Display for RouteResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.paths.is_empty() {
            if self.is_fuzzy {
                write!(
                    f,
                    "{}",
                    header(
                        "🔍",
                        &format!("未找到匹配「{}」的用户", dim(&self.query)),
                        ""
                    )
                )?;
            } else {
                write!(
                    f,
                    "{}",
                    header(
                        "🗺️",
                        &format!(
                            "未找到「{}」到「{}」的路径",
                            dim(&self.from),
                            dim(&self.query)
                        ),
                        ""
                    )
                )?;
            }
            return Ok(());
        }

        if self.is_fuzzy {
            // Fuzzy: per‑matched‑user cards.
            write!(
                f,
                "{}",
                header(
                    "🔍",
                    &format!("「{}」找到 {} 个模糊匹配", dim(&self.query), self.total),
                    ""
                )
            )?;

            for info in &self.paths {
                let target_login = info.path.last().map(|u| u.login.as_str()).unwrap_or("?");
                let chain = build_path_chain(info);
                let steps = dim(&format!("{} 步", info.path.len().saturating_sub(1)));
                let mut body = vec![format!("{chain}  {steps}")];
                if info.path.len() > 2 {
                    body.extend(build_edges(info));
                }
                write!(f, "\n\n{}", card(&blue(target_login), &body))?;
            }
        } else if self.total <= 1 && self.paths.len() == 1 {
            // Exact, single path.
            let info = &self.paths[0];
            let chain = build_path_chain(info);
            let steps = dim(&format!("{} 步", info.path.len().saturating_sub(1)));

            if info.path.len() > 2 && !info.directed_edges.is_empty() {
                let head = format!("{chain}  {steps}");
                let body = build_edges(info);
                write!(
                    f,
                    "{}",
                    header(
                        "🗺️",
                        &format!("{} 到 {}", dim(&self.from), bold(&self.query)),
                        ""
                    )
                )?;
                write!(f, "\n\n{}", card(&head, &body))?;
            } else {
                write!(
                    f,
                    "{}",
                    header(
                        "🗺️",
                        &format!("{} 到 {}", dim(&self.from), bold(&self.query)),
                        &format!("{chain}  {steps}")
                    )
                )?;
            }
        } else {
            // Exact, multiple paths.
            let meta = if self.total > self.paths.len() {
                format!("共 {} 条路径，显示前 {} 条", self.total, self.paths.len())
            } else {
                format!("共 {} 条路径", self.total)
            };
            write!(
                f,
                "{}",
                header(
                    "🗺️",
                    &format!("{} 到 {}", dim(&self.from), bold(&self.query)),
                    &meta
                )
            )?;

            for info in &self.paths {
                let chain = build_path_chain(info);
                let steps = dim(&format!("{} 步", info.path.len().saturating_sub(1)));
                let head = format!("{chain}  {steps}");
                let body = build_edges(info);
                write!(f, "\n\n{}", card(&head, &body))?;
            }
        }
        Ok(())
    }
}

// ── CommonResult ──────────────────────────────────────────────────────────

impl fmt::Display for CommonResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            header(
                "👥",
                &format!("{} 和 {}", blue(&self.user1), blue(&self.user2)),
                ""
            )
        )?;

        for (label, list) in [
            ("共同关注", &self.common_following),
            ("共同粉丝", &self.common_followers),
        ] {
            write!(f, "\n\n{}", tree_title_list(label, list))?;
        }

        Ok(())
    }
}

// ── UserProfileResult ─────────────────────────────────────────────────────

/// Wrapper to control the `--detail` flag for user view.
pub struct UserView<'a> {
    pub data: &'a UserProfileResult,
    /// If true, show all names instead of truncating.
    pub detail: bool,
}

impl fmt::Display for UserView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let d = self.data;

        write!(f, "{}", header("👤", &blue(&d.login), ""))?;

        // ── Basic info ──
        let na = dim("—");
        let profile_items = [
            TreeItem::new("姓名", d.name.as_deref().unwrap_or(&na)),
            TreeItem::new("公司", d.company.as_deref().unwrap_or(&na)),
            TreeItem::new("所在地", d.location.as_deref().unwrap_or(&na)),
            TreeItem::new(
                "账号创建",
                d.created_at
                    .as_ref()
                    .map(|s| &s[..s.len().min(10)])
                    .unwrap_or(&na),
            ),
        ];
        write!(f, "\n\n{}", tree_block("基本信息", &profile_items))?;

        // ── Stats ──
        let f_num = |n: Option<i64>| n.map(|v| num(v as u64)).unwrap_or_else(|| dim("—"));
        let stat_items = [
            TreeItem::new(
                "关注",
                format!(
                    "{} 人  {}",
                    f_num(d.following_count),
                    dim(&format!("已获取 {} 人", d.crawled_following))
                ),
            ),
            TreeItem::new(
                "粉丝",
                format!(
                    "{} 人  {}",
                    f_num(d.followers_count),
                    dim(&format!("已获取 {} 人", d.crawled_followers))
                ),
            ),
            TreeItem::new("公开仓库", format!("{} 个", f_num(d.public_repos))),
        ];
        write!(f, "\n\n{}", tree_block("统计", &stat_items))?;

        // ── Social ──
        let max_names = if self.detail { usize::MAX } else { 10 };
        let groups = social_groups(d, max_names);
        write!(f, "\n\n{}", nested_tree("社交关系", &groups))?;

        Ok(())
    }
}

// ── SuggestResult ─────────────────────────────────────────────────────────

impl fmt::Display for SuggestResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.based_on == 0 {
            write!(
                f,
                "{}",
                header(
                    "💡",
                    &format!("{} 没有关注任何人，无法推荐", dim(&self.user)),
                    ""
                )
            )?;
            return Ok(());
        }
        if self.suggestions.is_empty() {
            write!(f, "{}", header("💡", "暂无推荐，试试先多爬些数据", ""))?;
            return Ok(());
        }

        write!(
            f,
            "{}",
            header(
                "💡",
                &format!(
                    "基于 {} 的社交圈推荐  top {}",
                    blue(&self.user),
                    self.suggestions.len()
                ),
                ""
            )
        )?;

        let max_weight = self.suggestions.first().map(|s| s.weight).unwrap_or(1.0);

        // Compute max login width for alignment.
        let max_login_w = self
            .suggestions
            .iter()
            .map(|s| UnicodeWidthStr::width(s.login.as_str()))
            .max()
            .unwrap_or(0);

        for s in &self.suggestions {
            let bar = weight_bar(s.weight, max_weight, 7);
            let login_w = UnicodeWidthStr::width(s.login.as_str());
            let login_pad = max_login_w.saturating_sub(login_w);
            let head = format!(
                "{}{}  {bar}  {:.2}",
                blue(&s.login),
                " ".repeat(login_pad),
                s.weight
            );

            let friends_line = if s.mutual_friends.is_empty() {
                dim("无")
            } else {
                let truncated = truncate_list(&s.mutual_friends, 3);
                format!("{truncated} 也关注了 ta")
            };
            let body = vec![friends_line];

            write!(f, "\n\n{}", card(&head, &body))?;
        }

        write!(
            f,
            "{}",
            footer(&format!(
                "基于 {} 个关注者，覆盖 {} 个候选",
                dim(&self.based_on.to_string()),
                dim(&num(self.candidates as u64))
            ))
        )?;

        Ok(())
    }
}

// ── BridgesResult ─────────────────────────────────────────────────────────

impl fmt::Display for BridgesResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.bridges.is_empty() {
            write!(f, "{}", header("🌉", "图中没有足够数据计算桥梁节点", ""))?;
            return Ok(());
        }

        write!(
            f,
            "{}",
            header(
                "🌉",
                &format!("桥梁节点  top {}", self.bridges.len()),
                &format!("隐藏后连通分量从 {} 增加", self.baseline_components)
            )
        )?;

        let headers = ["#", "login", "关注", "粉丝", "关键性"];
        let rows: Vec<Vec<String>> = self
            .bridges
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let f = |n: i64| num(n as u64);
                vec![
                    format!("#{}", i + 1),
                    blue(&b.login),
                    f(b.following.unwrap_or(0)),
                    f(b.followers.unwrap_or(0)),
                    impact_color(b.impact),
                ]
            })
            .collect();

        write!(f, "\n\n{}", align_grid(&headers, &rows))?;

        Ok(())
    }
}

// ── CommunitiesResult ─────────────────────────────────────────────────────

impl fmt::Display for CommunitiesResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // --user mode.
        if let Some(ref members) = self.user_members {
            let cid = self.user_community.unwrap_or(0);
            let head = format!("#{cid}   {} 人", num(members.len() as u64));
            let reps: Vec<String> = members.iter().take(3).cloned().collect();
            let body = vec![format!("{} 为代表", reps.join(", "))];

            let title = format!(
                "{} 所在社区",
                blue(self.user_login.as_deref().unwrap_or("?"))
            );
            write!(f, "{}", header("🏘️", &title, ""))?;
            write!(f, "\n\n{}", card(&head, &body))?;

            // Full member list for grep.
            write!(f, "\n\n  同社区成员:")?;
            for m in members {
                write!(f, "\n    {m}")?;
            }
            return Ok(());
        }

        if self.communities.is_empty() {
            write!(f, "{}", header("🏘️", "图中没有检测到社区", ""))?;
            return Ok(());
        }

        write!(
            f,
            "{}",
            header(
                "🏘️",
                &format!(
                    "社区发现  {} 算法，模块度 Q={}",
                    self.algorithm,
                    modularity_color(self.modularity)
                ),
                &format!("共 {} 个社区", self.num_communities)
            )
        )?;

        for c in &self.communities {
            let head = format!("#{}   {} 人", c.id + 1, num(c.size as u64));
            let body = vec![format!("{} 为代表", c.representatives.join(", "))];
            write!(f, "\n\n{}", card(&head, &body))?;
        }

        write!(
            f,
            "{}",
            footer(&format!("仅显示前 {} 个社区", self.communities.len()))
        )?;

        Ok(())
    }
}

// ── StatsResult ───────────────────────────────────────────────────────────

impl fmt::Display for StatsResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", header("📊", "数据库概况", ""))?;

        let size_str = if self.file_size_bytes > 1_000_000 {
            format!("{:.1} MB", self.file_size_bytes as f64 / 1_000_000.0)
        } else {
            format!("{} KB", self.file_size_bytes / 1000)
        };

        let overview_rows = [
            ("用户总数", num(self.total_users)),
            ("已爬", num(self.crawled)),
            ("排队", num(self.pending)),
            (
                "度数范围",
                format!("{}° ~ {}°", self.min_degree, self.max_degree),
            ),
            ("数据库", size_str),
        ];
        write!(f, "\n\n{}", kv_grid(&overview_rows))?;

        // Degree distribution.
        write!(f, "\n\n{}", section("度数分布"))?;
        if self.degree_dist.is_empty() {
            write!(f, "\n  {}", dim("（无数据）"))?;
        } else {
            let max_count = self.degree_dist.iter().map(|d| d.count).max().unwrap_or(1) as u64;
            for d in &self.degree_dist {
                let b = bar(d.count as u64, max_count, 30);
                let deg = cyan(&d.degree.to_string());
                let cnt = num(d.count as u64);
                write!(f, "\n    {deg}°  {cnt}  {b}")?;
            }
        }

        // Graph stats.
        write!(f, "\n\n{}", section("图统计"))?;
        let graph_rows = [
            ("边数", num(self.total_edges)),
            ("图密度", density_color(self.density)),
            ("连通分量数", num(self.connected_components as u64)),
            (
                "最大分量占比",
                format!("{:.1}%", self.largest_component_ratio * 100.0),
            ),
            ("平均出度", format!("{:.2}", self.avg_out_degree)),
            ("平均入度", format!("{:.2}", self.avg_in_degree)),
            ("有出边的用户", num(self.users_with_outgoing)),
            ("有入边的用户", num(self.users_with_incoming)),
        ];
        write!(f, "\n{}", kv_grid(&graph_rows))?;

        Ok(())
    }
}

// ── StatusData (for `gh6 status`) ─────────────────────────────────────────

impl fmt::Display for StatusData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use chrono::{Local, TimeZone};

        let state_str = if self.paused {
            "⏸ 已暂停".to_string()
        } else {
            "▶ 运行中".to_string()
        };

        let api_str = format!(
            "{} / {}",
            num(self.api_remaining as u64),
            num(self.api_limit as u64)
        );
        let currently = self.currently_crawling.as_deref().unwrap_or("（空闲）");

        let reset_str = if self.api_reset_at == 0 {
            "（未知）".to_string()
        } else {
            let local = Local
                .timestamp_opt(self.api_reset_at, 0)
                .single()
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "?".to_string());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let remaining = (self.api_reset_at - now).max(0);
            let rel = if remaining < 60 {
                format!("{remaining}s")
            } else if remaining < 3600 {
                format!("{}m {}s", remaining / 60, remaining % 60)
            } else {
                let h = remaining / 3600;
                let m = (remaining % 3600) / 60;
                format!("{h}h {m}m")
            };
            format!("{local} (in {rel})")
        };

        let uptime = fmt_uptime(self.uptime_secs);

        let title = format!("⏳ gh6 · {}", dim(&uptime));
        writeln!(f, "{title}")?;

        let rows = [
            ("服务状态", state_str),
            ("已爬", num(self.users_crawled)),
            ("重试", num(self.users_retry)),
            ("错误", num(self.users_error)),
            ("队列", num(self.users_queued)),
            ("当前度数", format!("{}°", self.current_degree)),
            ("正在爬取", currently.to_string()),
            ("API 剩余", api_str),
            ("下次重置", reset_str),
            ("运行时间", uptime),
        ];
        write!(f, "{}", kv_grid(&rows))?;

        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Simple KV grid (no headers) — used by stats and status.
fn kv_grid(items: &[(&str, String)]) -> String {
    let max_key_w = items
        .iter()
        .map(|(k, _)| UnicodeWidthStr::width(*k))
        .max()
        .unwrap_or(0);

    let mut s = String::new();
    for (i, (k, v)) in items.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        let kw = UnicodeWidthStr::width(*k);
        let pad = max_key_w.saturating_sub(kw);
        s.push_str(&format!("  {k}{}  {v}", " ".repeat(pad)));
    }
    s
}

/// Render a titled tree list (used by common view).
fn tree_title_list(label: &str, list: &[String]) -> String {
    let title = format!("{label} {} 人", list.len());
    if list.is_empty() {
        return format!("  {title}\n  └ {}", dim("无"));
    }
    let mut s = format!("  {title}");
    for (i, item) in list.iter().enumerate() {
        s.push('\n');
        let prefix = if i == list.len() - 1 { "└ " } else { "├ " };
        s.push_str(&format!("  {prefix}{item}"));
    }
    s
}

/// Build a styled path chain from a `PathInfo`.
fn build_path_chain(info: &PathInfo) -> String {
    let logins: Vec<&str> = info.path.iter().map(|u| u.login.as_str()).collect();
    path_chain(&logins)
}

/// Build directed‑edge lines from a `PathInfo`.
fn build_edges(info: &PathInfo) -> Vec<String> {
    info.directed_edges
        .iter()
        .map(|e| directed_edge(&e.from, &e.to))
        .collect()
}

/// Build social‑section tree groups for user view.
fn social_groups(data: &UserProfileResult, max_names: usize) -> Vec<TreeGroup> {
    let following_set: std::collections::HashSet<&str> =
        data.following.iter().map(|s| s.as_str()).collect();
    let mutual_set: std::collections::HashSet<&str> =
        data.mutual.iter().map(|s| s.as_str()).collect();

    let following_only: Vec<String> = data
        .following
        .iter()
        .filter(|s| !mutual_set.contains(s.as_str()))
        .cloned()
        .collect();
    let followers_only: Vec<String> = data
        .followers
        .iter()
        .filter(|s| !following_set.contains(s.as_str()))
        .cloned()
        .collect();

    let mut groups = Vec::new();

    if !following_only.is_empty() || !data.mutual.is_empty() || !followers_only.is_empty() {
        if !following_only.is_empty() {
            let label = format!("{} 关注 {} 人", green("→"), following_only.len());
            let items = truncate_list_items(&following_only, max_names);
            groups.push(TreeGroup { label, items });
        }
        if !data.mutual.is_empty() {
            let label = format!("{} 互关 {} 人", yellow("⇄"), data.mutual.len());
            let items = truncate_list_items(&data.mutual, max_names);
            groups.push(TreeGroup { label, items });
        }
        if !followers_only.is_empty() {
            let label = format!("{} 粉丝 {} 人", cyan("←"), followers_only.len());
            let items = truncate_list_items(&followers_only, max_names);
            groups.push(TreeGroup { label, items });
        }
    }

    groups
}

/// Split a list into display items for truncate_list.
fn truncate_list_items(items: &[String], max: usize) -> Vec<String> {
    if items.len() <= max {
        items.to_vec()
    } else {
        let mut out: Vec<String> = items.iter().take(max).cloned().collect();
        let remaining = items.len() - max;
        out.push(format!("等 {} 人", remaining));
        out
    }
}
