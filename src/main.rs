//! revert-radar: find reverted commits in a git repository and summarize them.
//! Zero dependencies; shells out to `git log`.

use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal;
use std::process::{exit, Command};

const USAGE: &str = "\
revert-radar - 扫描 git 历史里的回滚（revert）提交

用法:
  revert-radar [仓库路径] [选项]

选项:
  --since <时间>       只看这之后的提交，原样传给 git（如 \"90 days ago\"、2026-01-01）
  --until <时间>       只看这之前的提交
  --ref <分支/提交>    扫描的起点，默认 HEAD
  --top <N>            热点文件、明细列表各显示前 N 条，默认 10
  --fast <小时>        多快算“秒回滚”，默认 24
  --authors            额外列出被回滚提交的作者（默认不列，避免变成追责表）
  --json               输出 JSON
  --html <文件>        另存一份带图表的单文件 HTML 报告
  --fail-over <百分比> 回滚率超过该值时退出码为 2，便于放进 CI
  --no-color           不要彩色输出
  -h, --help           显示帮助
  -V, --version        显示版本
";

#[derive(Debug, Clone)]
struct Commit {
    sha: String,
    author: String,
    time: i64,
    subject: String,
    body: String,
    files: Vec<String>,
}

#[derive(Debug, Clone)]
struct Revert {
    revert: usize,           // index into commits
    original: Option<usize>, // index of the reverted commit, if found in range
    original_sha: Option<String>,
    reland: bool,            // the reverted commit was itself a revert
}

struct Opts {
    path: String,
    since: Option<String>,
    until: Option<String>,
    git_ref: String,
    top: usize,
    fast_hours: f64,
    authors: bool,
    json: bool,
    html: Option<String>,
    fail_over: Option<f64>,
    color: bool,
}

fn parse_args() -> Opts {
    let mut o = Opts {
        path: ".".into(),
        since: None,
        until: None,
        git_ref: "HEAD".into(),
        top: 10,
        fast_hours: 24.0,
        authors: false,
        json: false,
        html: None,
        fail_over: None,
        color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let need = |i: usize, name: &str| -> String {
        args.get(i + 1).cloned().unwrap_or_else(|| die(&format!("{name} 需要一个参数")))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{USAGE}");
                exit(0);
            }
            "-V" | "--version" => {
                println!("revert-radar {}", env!("CARGO_PKG_VERSION"));
                exit(0);
            }
            "--since" => { o.since = Some(need(i, a)); i += 1; }
            "--until" => { o.until = Some(need(i, a)); i += 1; }
            "--ref" => { o.git_ref = need(i, a); i += 1; }
            "--top" => {
                o.top = need(i, a).parse().unwrap_or_else(|_| die("--top 要是正整数"));
                i += 1;
            }
            "--fast" => {
                o.fast_hours = need(i, a).parse().unwrap_or_else(|_| die("--fast 要是数字（小时）"));
                i += 1;
            }
            "--fail-over" => {
                let v = need(i, a);
                let v = v.trim_end_matches('%');
                o.fail_over = Some(v.parse().unwrap_or_else(|_| die("--fail-over 要是数字（百分比）")));
                i += 1;
            }
            "--html" => { o.html = Some(need(i, a)); i += 1; }
            "--authors" => o.authors = true,
            "--json" => o.json = true,
            "--no-color" => o.color = false,
            s if s.starts_with('-') => die(&format!("不认识的选项: {s}（--help 看用法）")),
            s => o.path = s.to_string(),
        }
        i += 1;
    }
    if o.json {
        o.color = false;
    }
    o
}

fn die(msg: &str) -> ! {
    eprintln!("revert-radar: {msg}");
    exit(1);
}

fn load_commits(o: &Opts) -> Vec<Commit> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&o.path).args([
        "log",
        "--no-merges",
        "--no-color",
        "--format=%x1e%H%x1f%an%x1f%at%x1f%s%x1f%b%x1f",
        "--name-only",
    ]);
    if let Some(s) = &o.since { cmd.arg(format!("--since={s}")); }
    if let Some(u) = &o.until { cmd.arg(format!("--until={u}")); }
    cmd.arg(&o.git_ref).arg("--");
    let out = cmd.output().unwrap_or_else(|e| die(&format!("运行 git 失败: {e}")));
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("bad revision 'HEAD'") || err.contains("does not have any commits") {
            die("这个仓库还没有任何提交");
        }
        die(&format!("git log 出错: {}", err.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for rec in text.split('\x1e').skip(1) {
        let parts: Vec<&str> = rec.splitn(6, '\x1f').collect();
        if parts.len() < 6 {
            continue;
        }
        commits.push(Commit {
            sha: parts[0].to_string(),
            author: parts[1].to_string(),
            time: parts[2].parse().unwrap_or(0),
            subject: parts[3].to_string(),
            body: parts[4].to_string(),
            files: parts[5].lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        });
    }
    commits // newest first
}

/// Pulls the sha out of "This reverts commit <sha>".
fn reverted_sha(body: &str) -> Option<String> {
    let key = "This reverts commit ";
    let start = body.find(key)? + key.len();
    let sha: String = body[start..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if sha.len() >= 7 { Some(sha.to_lowercase()) } else { None }
}

/// `Revert "foo"` -> `foo`; handles nested `Revert "Revert "foo""`.
fn quoted_subject(subject: &str) -> Option<&str> {
    let s = subject.strip_prefix("Revert \"")?;
    s.strip_suffix('"')
}

fn is_revert(c: &Commit) -> bool {
    c.subject.starts_with("Revert \"")
        || c.subject.starts_with("Revert:")
        || c.subject.starts_with("Reapply \"") || reverted_sha(&c.body).is_some()
}

fn find_reverts(commits: &[Commit]) -> Vec<Revert> {
    let by_sha: HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
    let mut out = Vec::new();
    for (i, c) in commits.iter().enumerate() {
        if !is_revert(c) {
            continue;
        }
        let sha = reverted_sha(&c.body);
        let mut original = sha.as_deref().and_then(|s| {
            by_sha.get(s).copied().or_else(|| commits.iter().position(|x| x.sha.starts_with(s)))
        });
        if original.is_none() {
            // Fall back to matching the quoted subject among older commits.
            if let Some(q) = quoted_subject(&c.subject) {
                original = commits.iter().enumerate().skip(i + 1).find(|(_, x)| x.subject == q).map(|(j, _)| j);
            }
        }
        let reland = original.map(|j| is_revert(&commits[j])).unwrap_or(false)
            || c.subject.starts_with("Revert \"Revert ")
            || c.subject.starts_with("Reapply \"");
        out.push(Revert { revert: i, original, original_sha: sha, reland });
    }
    out
}

// ---------- dates without a date crate ----------

fn civil(ts: i64, offset_secs: i64) -> (i64, u32, u32) {
    let days = (ts + offset_secs).div_euclid(86_400);
    // Howard Hinnant's days_from_civil inverse
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn local_offset() -> i64 {
    // Use the offset `date` reports; fall back to UTC.
    Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.len() != 5 { return None; }
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let h: i64 = s[1..3].parse().ok()?;
            let m: i64 = s[3..5].parse().ok()?;
            Some(sign * (h * 3600 + m * 60))
        })
        .unwrap_or(0)
}

fn fmt_dur(secs: i64) -> String {
    let s = secs.max(0);
    if s < 3600 {
        format!("{} 分钟", (s / 60).max(1))
    } else if s < 86_400 {
        format!("{:.1} 小时", s as f64 / 3600.0)
    } else {
        format!("{:.1} 天", s as f64 / 86_400.0)
    }
}

fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    Some(sorted[idx])
}

// ---------- report ----------

struct Report {
    total: usize,
    reverts: Vec<Revert>,
    ttr: Vec<i64>, // sorted seconds
    buckets: [(&'static str, usize); 5],
    months: BTreeMap<String, (usize, usize)>, // month -> (commits, reverts)
    hot_files: Vec<(String, usize)>,
    authors: Vec<(String, usize)>,
    fast: usize,
    relands: usize,
    unmatched: usize,
    first: i64,
    last: i64,
}

fn build_report(commits: &[Commit], reverts: Vec<Revert>, o: &Opts, off: i64) -> Report {
    let mut ttr: Vec<i64> = reverts
        .iter()
        .filter_map(|r| r.original.map(|j| commits[r.revert].time - commits[j].time))
        .collect();
    ttr.sort_unstable();

    let mut buckets = [("1 小时内", 0), ("1 天内", 0), ("1 周内", 0), ("30 天内", 0), ("更久", 0)];
    for &t in &ttr {
        let k = match t {
            t if t < 3600 => 0,
            t if t < 86_400 => 1,
            t if t < 7 * 86_400 => 2,
            t if t < 30 * 86_400 => 3,
            _ => 4,
        };
        buckets[k].1 += 1;
    }

    let mut months: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for c in commits {
        let (y, m, _) = civil(c.time, off);
        months.entry(format!("{y}-{m:02}")).or_default().0 += 1;
    }
    for r in &reverts {
        let (y, m, _) = civil(commits[r.revert].time, off);
        months.entry(format!("{y}-{m:02}")).or_default().1 += 1;
    }

    let mut files: HashMap<&str, usize> = HashMap::new();
    let mut authors: HashMap<&str, usize> = HashMap::new();
    for r in &reverts {
        let rc = &commits[r.revert];
        let list = if rc.files.is_empty() {
            r.original.map(|j| &commits[j].files).unwrap_or(&rc.files)
        } else {
            &rc.files
        };
        for f in list {
            *files.entry(f.as_str()).or_default() += 1;
        }
        if let Some(j) = r.original {
            *authors.entry(commits[j].author.as_str()).or_default() += 1;
        }
    }
    let rank = |m: HashMap<&str, usize>| {
        let mut v: Vec<(String, usize)> = m.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    };

    let fast_secs = (o.fast_hours * 3600.0) as i64;
    Report {
        total: commits.len(),
        fast: ttr.iter().filter(|&&t| t < fast_secs).count(),
        relands: reverts.iter().filter(|r| r.reland).count(),
        unmatched: reverts.iter().filter(|r| r.original.is_none()).count(),
        first: commits.last().map(|c| c.time).unwrap_or(0),
        last: commits.first().map(|c| c.time).unwrap_or(0),
        reverts,
        ttr,
        buckets,
        months,
        hot_files: rank(files),
        authors: rank(authors),
    }
}

impl Report {
    fn rate(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.reverts.len() as f64 * 100.0 / self.total as f64 }
    }
}

struct Paint(bool);
impl Paint {
    fn c(&self, code: &str, s: &str) -> String {
        if self.0 { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
}

fn bar(n: usize, max: usize, width: usize) -> String {
    if max == 0 || n == 0 {
        return String::new();
    }
    let w = ((n as f64 / max as f64) * width as f64).ceil() as usize;
    "█".repeat(w.max(1))
}

/// Pads to a display width, counting CJK and full-width chars as 2 columns.
fn pad(s: &str, width: usize) -> String {
    let w: usize = s.chars().map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 }).sum();
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

fn short(s: &str) -> &str { &s[..s.len().min(8)] }

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).collect::<String>() + "…" }
}

fn date_str(ts: i64, off: i64) -> String {
    let (y, m, d) = civil(ts, off);
    format!("{y}-{m:02}-{d:02}")
}

fn print_text(r: &Report, commits: &[Commit], o: &Opts, off: i64) {
    let p = Paint(o.color);
    println!("{}", p.c("1", "revert-radar"));
    if r.total == 0 {
        println!("这个范围内没有提交。");
        return;
    }
    println!(
        "范围 {} ~ {}，共 {} 个提交（不含合并提交）",
        date_str(r.first, off), date_str(r.last, off), r.total
    );
    let rate = r.rate();
    let rate_s = format!("{:.2}%", rate);
    let rate_c = if rate >= 5.0 { "31" } else if rate >= 2.0 { "33" } else { "32" };
    println!("回滚提交 {} 个，回滚率 {}", p.c("1", &r.reverts.len().to_string()), p.c(rate_c, &rate_s));
    if r.reverts.is_empty() {
        println!("没发现回滚，挺稳。");
        return;
    }
    if let (Some(med), Some(p90)) = (percentile(&r.ttr, 0.5), percentile(&r.ttr, 0.9)) {
        println!(
            "从提交到被回滚：中位数 {}，P90 {}，最快 {}",
            fmt_dur(med), fmt_dur(p90), fmt_dur(r.ttr[0])
        );
    }
    println!(
        "{} 小时内就回滚的 {} 个；回滚的回滚（重新合入）{} 个；找不到原提交的 {} 个",
        o.fast_hours, r.fast, r.relands, r.unmatched
    );

    if !r.ttr.is_empty() {
        println!("\n{}", p.c("1", "多久被回滚"));
        let max = r.buckets.iter().map(|b| b.1).max().unwrap_or(0);
        for (label, n) in r.buckets {
            println!("  {} {:>4}  {}", pad(label, 8), n, p.c("36", &bar(n, max, 30)));
        }
    }

    println!("\n{}", p.c("1", "按月（提交数 / 回滚数）"));
    let max = r.months.values().map(|v| v.1).max().unwrap_or(0);
    let months: Vec<_> = r.months.iter().collect();
    let skip = months.len().saturating_sub(18);
    if skip > 0 {
        println!("  （只显示最近 18 个月）");
    }
    for (m, (c, n)) in months.into_iter().skip(skip) {
        println!("  {m}  {:>5} / {:<3} {}", c, n, p.c("33", &bar(*n, max, 30)));
    }

    if !r.hot_files.is_empty() {
        println!("\n{}", p.c("1", "最常被回滚牵连的文件"));
        for (f, n) in r.hot_files.iter().take(o.top) {
            println!("  {:>3}  {}", n, f);
        }
    }

    if o.authors && !r.authors.is_empty() {
        println!("\n{}", p.c("1", "被回滚提交的作者"));
        for (a, n) in r.authors.iter().take(o.top) {
            println!("  {:>3}  {}", n, a);
        }
    }

    println!("\n{}", p.c("1", "最近的回滚"));
    for rv in r.reverts.iter().take(o.top) {
        let c = &commits[rv.revert];
        let ttr = rv.original.map(|j| fmt_dur(c.time - commits[j].time)).unwrap_or_else(|| "?".into());
        let tag = if rv.reland { p.c("35", " [重新合入]") } else { String::new() };
        println!(
            "  {}  {}  {:>10}  {}{}",
            date_str(c.time, off), p.c("2", short(&c.sha)), ttr, trunc(&c.subject, 70), tag
        );
    }
}

fn esc_json(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn print_json(r: &Report, commits: &[Commit], o: &Opts) {
    let opt = |v: Option<i64>| v.map(|x| x.to_string()).unwrap_or_else(|| "null".into());
    let mut s = String::from("{\n");
    s += &format!("  \"commits\": {},\n  \"reverts\": {},\n", r.total, r.reverts.len());
    s += &format!("  \"revert_rate_pct\": {:.4},\n", r.rate());
    s += &format!(
        "  \"ttr_seconds\": {{\"median\": {}, \"p90\": {}, \"min\": {}}},\n",
        opt(percentile(&r.ttr, 0.5)), opt(percentile(&r.ttr, 0.9)), opt(r.ttr.first().copied())
    );
    s += &format!(
        "  \"fast_reverts\": {},\n  \"fast_threshold_hours\": {},\n  \"relands\": {},\n  \"unmatched\": {},\n",
        r.fast, o.fast_hours, r.relands, r.unmatched
    );
    let months: Vec<String> = r.months.iter()
        .map(|(m, (c, n))| format!("{{\"month\": {}, \"commits\": {c}, \"reverts\": {n}}}", esc_json(m)))
        .collect();
    s += &format!("  \"months\": [{}],\n", months.join(", "));
    let files: Vec<String> = r.hot_files.iter().take(o.top)
        .map(|(f, n)| format!("{{\"path\": {}, \"reverts\": {n}}}", esc_json(f)))
        .collect();
    s += &format!("  \"hot_files\": [{}],\n", files.join(", "));
    if o.authors {
        let a: Vec<String> = r.authors.iter().take(o.top)
            .map(|(f, n)| format!("{{\"author\": {}, \"reverted\": {n}}}", esc_json(f)))
            .collect();
        s += &format!("  \"authors\": [{}],\n", a.join(", "));
    }
    let items: Vec<String> = r.reverts.iter().map(|rv| {
        let c = &commits[rv.revert];
        let orig = rv.original.map(|j| esc_json(&commits[j].sha))
            .or_else(|| rv.original_sha.as_deref().map(esc_json))
            .unwrap_or_else(|| "null".into());
        let ttr = rv.original.map(|j| (c.time - commits[j].time).to_string()).unwrap_or_else(|| "null".into());
        format!(
            "    {{\"sha\": {}, \"time\": {}, \"subject\": {}, \"original\": {}, \"ttr_seconds\": {}, \"reland\": {}}}",
            esc_json(&c.sha), c.time, esc_json(&c.subject), orig, ttr, rv.reland
        )
    }).collect();
    s += &format!("  \"items\": [\n{}\n  ]\n}}", items.join(",\n"));
    println!("{s}");
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn write_html(path: &str, r: &Report, commits: &[Commit], o: &Opts, off: i64, repo_name: &str) {
    // Monthly chart: commits as light bars, reverts as dark bars on a second scale.
    let months: Vec<_> = r.months.iter().collect();
    let n = months.len().max(1);
    let (w, h, pad) = (760.0, 200.0, 28.0);
    let bw = (w - pad * 2.0) / n as f64;
    let maxc = months.iter().map(|m| m.1 .0).max().unwrap_or(1).max(1) as f64;
    let maxr = months.iter().map(|m| m.1 .1).max().unwrap_or(1).max(1) as f64;
    let mut svg = format!("<svg viewBox=\"0 0 {w} {}\" role=\"img\" aria-label=\"每月提交与回滚\">", h + 40.0);
    for (i, (m, (c, rv))) in months.iter().enumerate() {
        let x = pad + i as f64 * bw;
        let ch = (*c as f64 / maxc) * h;
        let rh = (*rv as f64 / maxr) * h;
        svg += &format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" class=\"c\"><title>{m}：{c} 个提交</title></rect>",
            x + 1.0, h - ch + 10.0, (bw - 2.0).max(1.0), ch
        );
        if *rv > 0 {
            svg += &format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" class=\"r\"><title>{m}：{rv} 个回滚</title></rect>",
                x + bw * 0.25, h - rh + 10.0, (bw * 0.5).max(1.0), rh
            );
        }
        if n <= 24 || i % (n / 12 + 1) == 0 {
            svg += &format!(
                "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>",
                x + bw / 2.0, h + 30.0, &m[2..]
            );
        }
    }
    svg += "</svg>";

    let maxb = r.buckets.iter().map(|b| b.1).max().unwrap_or(0).max(1);
    let buckets: String = r.buckets.iter().map(|(l, c)| {
        format!(
            "<div class=\"b\"><span>{l}</span><i style=\"width:{:.0}%\"></i><b>{c}</b></div>",
            *c as f64 * 100.0 / maxb as f64
        )
    }).collect();

    let files: String = r.hot_files.iter().take(o.top)
        .map(|(f, c)| format!("<tr><td>{c}</td><td><code>{}</code></td></tr>", esc_html(f)))
        .collect();
    let authors: String = if o.authors {
        let rows: String = r.authors.iter().take(o.top)
            .map(|(a, c)| format!("<tr><td>{c}</td><td>{}</td></tr>", esc_html(a)))
            .collect();
        format!("<h2>被回滚提交的作者</h2><table>{rows}</table>")
    } else { String::new() };
    let items: String = r.reverts.iter().map(|rv| {
        let c = &commits[rv.revert];
        let ttr = rv.original.map(|j| fmt_dur(c.time - commits[j].time)).unwrap_or_else(|| "?".into());
        format!(
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}{}</td></tr>",
            date_str(c.time, off), short(&c.sha), ttr, esc_html(&c.subject),
            if rv.reland { " <em>重新合入</em>" } else { "" }
        )
    }).collect();
    let med = percentile(&r.ttr, 0.5).map(fmt_dur).unwrap_or_else(|| "-".into());

    let html = format!(r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>回滚报告 · {name}</title>
<style>
:root{{--bg:#fbfaf7;--fg:#1d1d1b;--mute:#76736c;--line:#e6e2d9;--c:#d9d3c5;--r:#c2410c}}
@media (prefers-color-scheme:dark){{:root{{--bg:#151513;--fg:#ecebe6;--mute:#9a978f;--line:#2c2b27;--c:#3a3832;--r:#fb923c}}}}
body{{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 -apple-system,"PingFang SC","Noto Sans SC",sans-serif}}
main{{max-width:820px;margin:0 auto;padding:32px 20px 60px}}
h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:16px;margin:32px 0 10px}}
.sub{{color:var(--mute)}}
.kpi{{display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin:22px 0}}
.kpi div{{border:1px solid var(--line);border-radius:10px;padding:12px}}
.kpi b{{display:block;font-size:22px}} .kpi span{{color:var(--mute);font-size:13px}}
svg{{width:100%;height:auto}} svg .c{{fill:var(--c)}} svg .r{{fill:var(--r)}} svg text{{font-size:10px;fill:var(--mute)}}
.b{{display:grid;grid-template-columns:80px 1fr 40px;align-items:center;gap:10px;margin:4px 0}}
.b i{{display:block;height:12px;background:var(--r);border-radius:3px;min-width:2px}} .b b{{text-align:right}}
table{{width:100%;border-collapse:collapse}} td{{border-top:1px solid var(--line);padding:6px 8px;vertical-align:top}}
td:first-child{{white-space:nowrap;color:var(--mute)}} em{{color:var(--r);font-style:normal;font-size:12px}}
code{{font:13px ui-monospace,Menlo,monospace}}
.legend span{{display:inline-block;width:10px;height:10px;margin:0 4px 0 12px;vertical-align:-1px}}
footer{{margin-top:40px;color:var(--mute);font-size:12px}}
@media (max-width:600px){{.kpi{{grid-template-columns:repeat(2,1fr)}}}}
</style></head><body><main>
<h1>回滚报告 · {name}</h1>
<div class="sub">{from} ~ {to}，{total} 个提交（不含合并提交）</div>
<div class="kpi">
<div><b>{rev}</b><span>回滚提交</span></div>
<div><b>{rate:.2}%</b><span>回滚率</span></div>
<div><b>{med}</b><span>提交到回滚（中位数）</span></div>
<div><b>{fast}</b><span>{fh} 小时内回滚</span></div>
</div>
<h2>每月提交与回滚</h2>
<div class="sub legend"><span style="background:var(--c)"></span>提交数<span style="background:var(--r)"></span>回滚数（两者各自按最大值缩放）</div>
{svg}
<h2>多久被回滚</h2>{buckets}
<h2>最常被回滚牵连的文件</h2><table>{files}</table>
{authors}
<h2>全部回滚（{rev}）</h2><table>{items}</table>
<footer>由 revert-radar {ver} 生成 · 回滚的回滚 {relands} 个 · 找不到原提交 {unmatched} 个</footer>
</main></body></html>
"#,
        name = esc_html(repo_name), from = date_str(r.first, off), to = date_str(r.last, off),
        total = r.total, rev = r.reverts.len(), rate = r.rate(), med = med, fast = r.fast,
        fh = o.fast_hours, svg = svg, buckets = buckets, files = files, authors = authors,
        items = items, ver = env!("CARGO_PKG_VERSION"), relands = r.relands, unmatched = r.unmatched,
    );
    std::fs::write(path, html).unwrap_or_else(|e| die(&format!("写 {path} 失败: {e}")));
}

fn repo_name(path: &str) -> String {
    Command::new("git")
        .args(["-C", path, "rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            p.rsplit('/').next().map(String::from)
        })
        .unwrap_or_else(|| "repo".into())
}

fn main() {
    let o = parse_args();
    let commits = load_commits(&o);
    let reverts = find_reverts(&commits);
    let off = local_offset();
    let report = build_report(&commits, reverts, &o, off);

    if o.json {
        print_json(&report, &commits, &o);
    } else {
        print_text(&report, &commits, &o, off);
    }
    if let Some(path) = &o.html {
        write_html(path, &report, &commits, &o, off, &repo_name(&o.path));
        if !o.json {
            println!("\nHTML 报告已写到 {path}");
        }
    }
    if let Some(limit) = o.fail_over {
        if report.rate() > limit {
            eprintln!("revert-radar: 回滚率 {:.2}% 超过阈值 {limit}%", report.rate());
            exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reverted_sha() {
        let b = "Broke CI.\n\nThis reverts commit 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b.\n";
        assert_eq!(reverted_sha(b).as_deref(), Some("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b"));
        assert_eq!(reverted_sha("This reverts commit abc."), None);
    }

    #[test]
    fn quoted_subjects() {
        assert_eq!(quoted_subject("Revert \"add cache\""), Some("add cache"));
        assert_eq!(quoted_subject("Revert \"Revert \"add cache\"\""), Some("Revert \"add cache\""));
        assert_eq!(quoted_subject("fix revert logic"), None);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0, 0), (1970, 1, 1));
        assert_eq!(civil(951_782_400, 0), (2000, 2, 29));
        assert_eq!(civil(1_792_425_600, 8 * 3600), (2026, 10, 20));
        assert_eq!(civil(1_792_425_600, 0), (2026, 10, 19));
    }

    fn mk(sha: &str, t: i64, subj: &str, body: &str) -> Commit {
        Commit { sha: sha.into(), author: "a".into(), time: t, subject: subj.into(), body: body.into(), files: vec![] }
    }

    #[test]
    fn matches_by_sha_subject_and_reland() {
        // newest first, as git log returns
        let cs = vec![
            mk("d4", 400, "Revert \"Revert \"feat\"\"", "This reverts commit c3."),
            mk("c3", 300, "Revert \"feat\"", "This reverts commit a1aaaaa."),
            mk("b2", 200, "Revert \"docs\"", ""),
            mk("a1aaaaaf", 100, "feat", ""),
            mk("a0", 50, "docs", ""),
        ];
        let rs = find_reverts(&cs);
        assert_eq!(rs.len(), 3);
        assert!(rs[0].reland);
        assert_eq!(rs[1].original, Some(3)); // prefix match
        assert_eq!(rs[2].original, Some(4)); // subject match
        assert!(!rs[1].reland);
    }
}
