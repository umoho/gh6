//! Terminal display primitives and views for gh6 analyze output.
//!
//! # Design
//!
//! All output flows through a small set of composable drawing primitives.
//! Each analyze result type implements [`fmt::Display`] by calling these
//! primitives — no ad‑hoc `println!()` anywhere.
//!
//! The core primitive is [`tree`]: every card, key‑value block, and nested
//! list is a special case of a tree with `├` / `└` / `│` connectors.

use std::fmt;

use owo_colors::OwoColorize;
use unicode_width::UnicodeWidthChar;

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

/// Explanatory suffix — always dimmed.
pub fn suffix(s: &str) -> String {
    dim(s)
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

// ── ANSI‑aware width helper ───────────────────────────────────────────────

/// Strip ANSI SGR escape sequences, then measure the visible display width.
pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip ESC [ ... m
            i += 2;
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            i += 1; // skip 'm'
        } else if let Some(c) = s[i..].chars().next() {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
            i += c.len_utf8();
        } else {
            break;
        }
    }
    width
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
/// Callers are responsible for styling `meta` (typically `dim()`).
/// Pass an empty `meta` to omit.
pub fn header(emoji: &str, title: &str, meta: &str) -> String {
    if meta.is_empty() {
        format!("{emoji} {title}")
    } else {
        format!("{emoji} {title}  {meta}")
    }
}

/// Dimmed footnote line.
pub fn footer(text: &str) -> String {
    format!("\n\n{text}")
}

// ── Tree primitive ────────────────────────────────────────────────────────

/// A node in the universal tree layout.
pub struct TreeNode {
    /// Pre‑formatted line content (styles already applied).
    pub content: String,
    /// Child nodes (empty = leaf).
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Create a leaf node (no children).
    pub fn leaf(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            children: vec![],
        }
    }

    /// Create a branch node with children.
    pub fn with_children(content: impl Into<String>, children: Vec<TreeNode>) -> Self {
        Self {
            content: content.into(),
            children,
        }
    }
}

/// Render a tree rooted at `root` with `├` / `└` / `│` connectors.
///
/// ```text
///   root
///   ├ item1
///   │ ├ item1.child1
///   │ └ item1.child2
///   └ item2
/// ```
pub fn tree(root: &str, items: &[TreeNode]) -> String {
    let mut s = format!("  {root}");
    if items.is_empty() {
        return s;
    }
    let last_idx = items.len() - 1;
    for (i, item) in items.iter().enumerate() {
        s.push('\n');
        s.push_str(&render_node(item, i == last_idx, &[]));
    }
    s
}

/// Recursively render a tree node at the current ancestor path.
fn render_node(node: &TreeNode, is_last: bool, ancestors: &[bool]) -> String {
    // Build the prefix from ancestor continuations.
    let prefix = ancestors
        .iter()
        .map(|&cont| if cont { "│ " } else { "  " })
        .collect::<String>();

    let branch = if is_last { "└ " } else { "├ " };
    let mut s = format!("  {prefix}{branch}{}", node.content);

    if !node.children.is_empty() {
        let mut next_ancestors = ancestors.to_vec();
        next_ancestors.push(!is_last);
        let last_idx = node.children.len() - 1;
        for (i, child) in node.children.iter().enumerate() {
            s.push('\n');
            s.push_str(&render_node(child, i == last_idx, &next_ancestors));
        }
    }
    s
}

// ── Grid primitive ────────────────────────────────────────────────────────

/// Borderless column‑aligned grid.  Headers are bold; row `#` is dim.
pub fn align_grid(headers: &[&str], rows: &[Vec<String>]) -> String {
    assert!(
        !headers.is_empty(),
        "align_grid requires at least one header"
    );
    let ncols = headers.len();

    // Compute column widths using visible width (ANSI‑aware).
    let mut widths: Vec<usize> = headers.iter().map(|h| visible_width(h)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                widths[i] = widths[i].max(visible_width(cell));
            }
        }
    }

    let mut s = String::new();

    // Header row (bold).
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            s.push_str("  ");
        }
        let w = visible_width(h);
        s.push_str(&bold(h));
        if i < ncols - 1 {
            let pad = widths[i].saturating_sub(w);
            s.push_str(&" ".repeat(pad));
        }
    }

    // Data rows.
    for row in rows {
        s.push('\n');
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            let w = visible_width(cell);
            s.push_str(cell);
            if i < ncols - 1 {
                let pad = widths[i].saturating_sub(w);
                s.push_str(&" ".repeat(pad));
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

/// Split a list into display items for use in tree nodes.
fn list_to_leaves(items: &[String], max: usize) -> Vec<TreeNode> {
    if items.len() <= max {
        items.iter().map(|s| TreeNode::leaf(s.clone())).collect()
    } else {
        let mut out: Vec<TreeNode> = items
            .iter()
            .take(max)
            .map(|s| TreeNode::leaf(s.clone()))
            .collect();
        let remaining = items.len() - max;
        out.push(TreeNode::leaf(dim(&format!("等 {} 人", remaining))));
        out
    }
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
                writeln!(
                    f,
                    "{}",
                    header(
                        "🔍",
                        &format!("未找到匹配「{}」的用户", dim(&self.query)),
                        ""
                    )
                )?;
            } else {
                writeln!(
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

        let meta = if self.total > self.paths.len() {
            dim(&format!("共 {} 条路径", self.total))
        } else if self.total == 1 {
            dim("共 1 条路径")
        } else {
            dim(&format!("共 {} 条路径", self.total))
        };

        if self.is_fuzzy {
            // Fuzzy: per‑matched‑user cards.
            writeln!(
                f,
                "{}",
                header(
                    "🔍",
                    &format!("「{}」找到 {} 个模糊匹配", dim(&self.query), self.total),
                    ""
                )
            )?;

            for (pi, info) in self.paths.iter().enumerate() {
                let target_login = info.path.last().map(|u| u.login.as_str()).unwrap_or("?");
                let chain = build_path_chain(info);
                let steps = dim(&format!("{} 步", info.path.len().saturating_sub(1)));

                let edge_nodes: Vec<TreeNode> = if info.path.len() > 2 {
                    info.directed_edges
                        .iter()
                        .map(|e| TreeNode::leaf(directed_edge(&e.from, &e.to)))
                        .collect()
                } else {
                    vec![]
                };

                let body = if !edge_nodes.is_empty() {
                    vec![TreeNode::with_children(
                        format!("{chain}  {steps}"),
                        edge_nodes,
                    )]
                } else {
                    vec![TreeNode::leaf(format!("{chain}  {steps}"))]
                };

                if pi > 0 {
                    write!(f, "\n\n")?;
                } else {
                    write!(f, "\n")?;
                }
                write!(f, "{}", tree(&blue(target_login), &body))?;
            }
        } else {
            // Exact match.
            writeln!(
                f,
                "{}",
                header(
                    "🗺️",
                    &format!("{} 到 {}", bold(&self.from), target(&self.query)),
                    &meta
                )
            )?;

            for (pi, info) in self.paths.iter().enumerate() {
                let chain = build_path_chain(info);
                let steps = dim(&format!("{} 步", info.path.len().saturating_sub(1)));

                let edge_nodes: Vec<TreeNode> = if info.path.len() > 2 {
                    info.directed_edges
                        .iter()
                        .map(|e| TreeNode::leaf(directed_edge(&e.from, &e.to)))
                        .collect()
                } else {
                    vec![]
                };

                let head = format!("{chain}  {steps}");

                if pi > 0 {
                    write!(f, "\n\n")?;
                } else {
                    write!(f, "\n")?;
                }
                write!(f, "{}", tree(&head, &edge_nodes))?;
            }
        }
        Ok(())
    }
}

// ── CommonResult ──────────────────────────────────────────────────────────

impl fmt::Display for CommonResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "{}",
            header(
                "👥",
                &format!("{} 和 {}", blue(&self.user1), blue(&self.user2)),
                ""
            )
        )?;

        for (pi, (label, list)) in [
            ("共同关注", &self.common_following),
            ("共同粉丝", &self.common_followers),
        ]
        .iter()
        .enumerate()
        {
            if pi > 0 {
                write!(f, "\n\n")?;
            } else {
                write!(f, "\n")?;
            }
            write!(f, "{}", tree_title_list(label, list))?;
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

        writeln!(f, "{}", header("👤", &blue(&d.login), ""))?;

        // ── Basic info ──
        let na = dim("—");
        let profile_items = [
            TreeNode::leaf(format!("姓名      {}", d.name.as_deref().unwrap_or(&na))),
            TreeNode::leaf(format!("公司      {}", d.company.as_deref().unwrap_or(&na))),
            TreeNode::leaf(format!(
                "所在地    {}",
                d.location.as_deref().unwrap_or(&na)
            )),
            TreeNode::leaf(format!(
                "账号创建  {}",
                d.created_at
                    .as_ref()
                    .map(|s| &s[..s.len().min(10)])
                    .unwrap_or(&na)
            )),
        ];
        write!(f, "\n{}", tree(&bold("基本信息"), &profile_items))?;

        // ── Stats (right‑aligned counts) ──
        let f_val = |n: Option<i64>, unit: &str| -> String {
            n.map(|v| format!("{} {}", num(v as u64), unit))
                .unwrap_or_else(|| na.clone())
        };
        let follow_val = f_val(d.following_count, "人");
        let follower_val = f_val(d.followers_count, "人");
        let repo_val = f_val(d.public_repos, "个");

        let max_w = [&follow_val, &follower_val, &repo_val]
            .iter()
            .map(|s| visible_width(s))
            .max()
            .unwrap_or(0);

        let max_crawled_w = [d.crawled_following, d.crawled_followers]
            .iter()
            .map(|n| format!("{n}").len())
            .max()
            .unwrap_or(0);

        let stat_nodes = vec![
            TreeNode::leaf(format!(
                "关注      {:>w$}  {}",
                follow_val,
                dim(&format!("已获取 {:>cw$} 人", d.crawled_following, cw = max_crawled_w)),
                w = max_w
            )),
            TreeNode::leaf(format!(
                "粉丝      {:>w$}  {}",
                follower_val,
                dim(&format!("已获取 {:>cw$} 人", d.crawled_followers, cw = max_crawled_w)),
                w = max_w
            )),
            TreeNode::leaf(format!("公开仓库  {:>w$}", repo_val, w = max_w)),
        ];
        write!(f, "\n\n{}", tree(&bold("统计"), &stat_nodes))?;

        // ── Social ──
        let max_names = if self.detail { usize::MAX } else { 10 };
        let groups = social_groups(d, max_names);
        write!(f, "\n\n{}", tree(&bold("社交关系"), &groups))?;

        Ok(())
    }
}

// ── SuggestResult ─────────────────────────────────────────────────────────

impl fmt::Display for SuggestResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.based_on == 0 {
            writeln!(
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
            writeln!(f, "{}", header("💡", "暂无推荐，试试先多爬些数据", ""))?;
            return Ok(());
        }

        writeln!(
            f,
            "{}",
            header(
                "💡",
                &format!(
                    "基于 {} 的社交圈推荐  {}",
                    blue(&self.user),
                    dim(&format!("top {}", self.suggestions.len()))
                ),
                ""
            )
        )?;

        let max_weight = self.suggestions.first().map(|s| s.weight).unwrap_or(1.0);

        // Compute max login width for alignment.
        let max_login_w = self
            .suggestions
            .iter()
            .map(|s| visible_width(s.login.as_str()))
            .max()
            .unwrap_or(0);

        for (si, s) in self.suggestions.iter().enumerate() {
            let bar = weight_bar(s.weight, max_weight, 7);
            let login_w = visible_width(s.login.as_str());
            let login_pad = max_login_w.saturating_sub(login_w);
            let head = format!(
                "{}{}  {bar}  {:.2}",
                blue(&s.login),
                " ".repeat(login_pad),
                s.weight
            );

            let friends_line = if s.mutual_friends.is_empty() {
                suffix("无")
            } else {
                let max_show = 3;
                let names: Vec<&str> = s
                    .mutual_friends
                    .iter()
                    .take(max_show)
                    .map(|n| n.as_str())
                    .collect();
                let remaining = s.mutual_friends.len().saturating_sub(max_show);
                if remaining == 0 {
                    format!("{} {}", names.join(", "), suffix("也关注了 ta"))
                } else {
                    format!(
                        "{} {}",
                        names.join(", "),
                        suffix(&format!("等 {} 人也关注了 ta", remaining))
                    )
                }
            };

            let body = vec![TreeNode::leaf(friends_line)];

            if si > 0 {
                write!(f, "\n\n")?;
            } else {
                write!(f, "\n")?;
            }
            write!(f, "{}", tree(&head, &body))?;
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
            writeln!(f, "{}", header("🌉", "图中没有足够数据计算桥梁节点", ""))?;
            return Ok(());
        }

        writeln!(
            f,
            "{}",
            header(
                "🌉",
                "桥梁节点",
                &format!("top {}", dim(&self.bridges.len().to_string()))
            )
        )?;
        writeln!(f, "隐藏后连通分量从 {} 增加", self.baseline_components)?;

        let headers = ["#", "login", "关注", "粉丝", "关键性"];
        let rows: Vec<Vec<String>> = self
            .bridges
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let f = |n: i64| num(n as u64);
                vec![
                    dim(&format!("#{}", i + 1)),
                    blue(&b.login),
                    f(b.following.unwrap_or(0)),
                    f(b.followers.unwrap_or(0)),
                    impact_color(b.impact),
                ]
            })
            .collect();

        write!(f, "\n{}", align_grid(&headers, &rows))?;

        Ok(())
    }
}

// ── CommunitiesResult ─────────────────────────────────────────────────────

impl fmt::Display for CommunitiesResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // --user mode.
        if let Some(ref members) = self.user_members {
            let cid = self.user_community.unwrap_or(0);
            let head = format!(
                "{}  {} 人",
                bold(&format!("社区 {}", cid)),
                num(members.len() as u64)
            );
            let reps = if members.is_empty() {
                suffix("无")
            } else {
                format!(
                    "{} {}",
                    members
                        .iter()
                        .take(3)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    suffix("为代表")
                )
            };
            let body = vec![TreeNode::leaf(reps)];

            writeln!(
                f,
                "{}",
                header(
                    "🏘️",
                    &format!(
                        "{} 所在社区",
                        blue(self.user_login.as_deref().unwrap_or("?"))
                    ),
                    ""
                )
            )?;
            write!(f, "\n{}", tree(&head, &body))?;

            // Full member list for grep.
            let member_nodes: Vec<TreeNode> = members
                .iter()
                .map(|m| TreeNode::leaf(m.clone()))
                .collect();
            write!(f, "\n\n{}", tree(&bold("同社区成员"), &member_nodes))?;
            return Ok(());
        }

        if self.communities.is_empty() {
            writeln!(f, "{}", header("🏘️", "图中没有检测到社区", ""))?;
            return Ok(());
        }

        writeln!(
            f,
            "{}",
            header(
                "🏘️",
                "社区发现",
                &format!("共 {} 个社区", dim(&self.num_communities.to_string()))
            )
        )?;
        writeln!(
            f,
            "\n  Louvain 算法  模块度 Q = {}",
            modularity_color(self.modularity)
        )?;

        for (ci, c) in self.communities.iter().enumerate() {
            let head = format!("{}  {} 人", bold(&format!("社区 {}", c.id)), num(c.size as u64));
            let reps = if c.representatives.is_empty() {
                suffix("无")
            } else {
                format!("{} {}", c.representatives.join(", "), suffix("为代表"))
            };
            let body = vec![TreeNode::leaf(reps)];

            if ci > 0 {
                write!(f, "\n\n")?;
            } else {
                write!(f, "\n")?;
            }
            write!(f, "{}", tree(&head, &body))?;
        }

        write!(
            f,
            "{}",
            footer(&format!(
                "仅显示前 {} 个社区",
                dim(&self.communities.len().to_string())
            ))
        )?;

        Ok(())
    }
}

// ── StatsResult ───────────────────────────────────────────────────────────

impl fmt::Display for StatsResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "{}", header("📊", "gh6 数据库", ""))?;

        // ── Database overview ──
        let size_str = if self.file_size_bytes > 1_000_000 {
            format!("{:.1} MB", self.file_size_bytes as f64 / 1_000_000.0)
        } else {
            format!("{} KB", self.file_size_bytes / 1000)
        };

        let overview_rows: &[(&str, String)] = &[
            ("用户总数", num(self.total_users)),
            ("已爬", num(self.crawled)),
            ("排队", num(self.pending)),
            (
                "度数范围",
                format!("{}° ~ {}°", self.min_degree, self.max_degree),
            ),
            ("数据库", size_str),
        ];
        let overview_nodes = kv_to_tree_nodes(overview_rows);
        write!(f, "\n{}", tree(&bold("数据库概况"), &overview_nodes))?;

        // ── Degree distribution ──
        write!(f, "\n\n  {}", bold("度数分布"))?;
        if self.degree_dist.is_empty() {
            write!(f, "\n  {}", dim("（无数据）"))?;
        } else {
            let max_count = self.degree_dist.iter().map(|d| d.count).max().unwrap_or(1) as u64;
            let max_cnt_w = self
                .degree_dist
                .iter()
                .map(|d| visible_width(&num(d.count as u64)))
                .max()
                .unwrap_or(0);
            for d in &self.degree_dist {
                let b = bar(d.count as u64, max_count, 30);
                let deg = cyan(&d.degree.to_string());
                let cnt = num(d.count as u64);
                let cnt_pad = max_cnt_w.saturating_sub(visible_width(&cnt));
                write!(f, "\n    {deg}°  {}{}  {b}", " ".repeat(cnt_pad), cnt)?;
            }
        }

        // ── Graph stats ──
        let graph_rows: &[(&str, String)] = &[
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
        let graph_nodes = kv_to_tree_nodes(graph_rows);
        write!(f, "\n\n{}", tree(&bold("图统计"), &graph_nodes))?;

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

        writeln!(f, "{}", header("⏳", "gh6", ""))?;

        let rows: &[(&str, String)] = &[
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
        let nodes = kv_to_tree_nodes(rows);
        write!(f, "\n{}", tree(&bold("状态"), &nodes))?;

        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Build KV tree nodes from key‑value pairs.
fn kv_to_tree_nodes(items: &[(&str, String)]) -> Vec<TreeNode> {
    let max_key_w = items
        .iter()
        .map(|(k, _)| visible_width(k))
        .max()
        .unwrap_or(0);

    items
        .iter()
        .map(|(k, v)| {
            let kw = visible_width(k);
            let pad = max_key_w.saturating_sub(kw);
            TreeNode::leaf(format!("{k}{}  {v}", " ".repeat(pad)))
        })
        .collect()
}

/// Render a titled tree list (used by common view).
fn tree_title_list(label: &str, list: &[String]) -> String {
    let title = format!("{} {}", bold(label), suffix(&format!("{} 人", list.len())));
    if list.is_empty() {
        return tree(&title, &[TreeNode::leaf(dim("无"))]);
    }
    let items: Vec<TreeNode> = list.iter().map(|s| TreeNode::leaf(s.clone())).collect();
    tree(&title, &items)
}

/// Build a styled path chain from a `PathInfo`.
fn build_path_chain(info: &PathInfo) -> String {
    let logins: Vec<&str> = info.path.iter().map(|u| u.login.as_str()).collect();
    path_chain(&logins)
}

/// Build social‑section tree groups for user view.
fn social_groups(data: &UserProfileResult, max_names: usize) -> Vec<TreeNode> {
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
            let label = format!(
                "{} {} {}",
                green("→"),
                bold("关注"),
                dim(&format!("{} 人", following_only.len()))
            );
            let items = list_to_leaves(&following_only, max_names);
            groups.push(TreeNode::with_children(label, items));
        }
        if !data.mutual.is_empty() {
            let label = format!(
                "{} {} {}",
                yellow("⇄"),
                bold("互关"),
                dim(&format!("{} 人", data.mutual.len()))
            );
            let items = list_to_leaves(&data.mutual, max_names);
            groups.push(TreeNode::with_children(label, items));
        }
        if !followers_only.is_empty() {
            let label = format!(
                "{} {} {}",
                cyan("←"),
                bold("粉丝"),
                dim(&format!("{} 人", followers_only.len()))
            );
            let items = list_to_leaves(&followers_only, max_names);
            groups.push(TreeNode::with_children(label, items));
        }
    }

    groups
}
