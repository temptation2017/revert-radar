# revert-radar

扫一遍 git 历史，把所有回滚（revert）提交找出来：回滚率多少、提交后多久被回滚、哪些文件老被牵连、有没有“回滚了又加回来”。单个 Rust 二进制，零依赖，只调用本机的 `git`。

**在线示例报告：** [temptation2017.github.io/revert-radar](https://temptation2017.github.io/revert-radar/)（用虚构仓库生成）

## 为什么做这个

现在不少代码是 AI 帮着写的，合进去快，回滚也快。回滚本身不是坏事，但如果某个目录一个月被回滚好几次，或者大部分回滚都发生在合入后几小时内，这通常说明评审、测试或灰度哪里漏了。这些信息其实都躺在 git 历史里，只是没人去数。

## 安装

需要 Rust 1.70+ 和 git。

```bash
cargo install --git https://github.com/temptation2017/revert-radar
```

或者克隆后自己编：

```bash
git clone https://github.com/temptation2017/revert-radar
cd revert-radar
cargo build --release
./target/release/revert-radar --help
```

## 用法

```bash
revert-radar                          # 扫当前目录所在仓库的 HEAD
revert-radar ~/code/my-app            # 指定仓库
revert-radar --since "90 days ago"    # 只看最近 90 天
revert-radar --ref origin/main        # 扫某个分支
revert-radar --html report.html       # 另存带图表的单文件报告
revert-radar --json > reverts.json    # 给脚本或看板用
revert-radar --fail-over 5            # 回滚率超过 5% 时退出码为 2
```

| 选项 | 说明 |
| --- | --- |
| `--since` / `--until` | 时间范围，原样传给 `git log`，支持 `2026-01-01`、`"3 months ago"` 等写法 |
| `--ref` | 扫描起点，默认 `HEAD` |
| `--top N` | 热点文件和明细各显示前 N 条，默认 10 |
| `--fast 小时` | 多快算“秒回滚”，默认 24 小时 |
| `--authors` | 额外列出被回滚提交的作者。默认不列，这个工具是用来找流程问题的，不是用来追责的 |
| `--json` | 输出 JSON |
| `--html 文件` | 生成单文件 HTML 报告（自动跟随系统深浅色，无外部资源） |
| `--fail-over 百分比` | 超过阈值时退出码 2，可放进 CI 当提醒 |
| `--no-color` | 关闭彩色；也认 `NO_COLOR` 环境变量 |

## 示例输出

下面是对仓库里自带的虚构示例仓库跑出来的结果（`scripts/make-demo-repo.sh` 生成）：

```text
revert-radar
范围 2026-04-05 ~ 2026-07-29，共 64 个提交（不含合并提交）
回滚提交 6 个，回滚率 9.38%
从提交到被回滚：中位数 12.0 小时，P90 20.0 天，最快 7 分钟
24 小时内就回滚的 4 个；回滚的回滚（重新合入）1 个；找不到原提交的 0 个

多久被回滚
  1 小时内    2  ██████████████████████████████
  1 天内      2  ██████████████████████████████
  1 周内      1  ███████████████
  30 天内     1  ███████████████
  更久        0  

按月（提交数 / 回滚数）
  2026-04     13 / 0   
  2026-05     16 / 0   
  2026-06     22 / 4   ██████████████████████████████
  2026-07     13 / 2   ███████████████

最常被回滚牵连的文件
    3  src/api/cache.go
    2  src/api/auth.go
    1  src/ui/theme.css

最近的回滚
  2026-07-29  84c7d6fa        7 分钟  Revert "ui: tune contrast"
  2026-07-29  a0e3d175      20.0 天  Revert "perf: cache warmup on boot"
  2026-06-27  13092bcf      2.0 小时  Reapply "fix: token refresh race" [重新合入]
  2026-06-27  44397d76     12.0 小时  Revert "fix: token refresh race"
  2026-06-26  5e8a5c07       3.0 天  Revert "feat: add response cache (take 2)"
  2026-06-02  2b4425ff       30 分钟  Revert "feat: add response cache"
```

想自己试一下：

```bash
scripts/make-demo-repo.sh /tmp/demo
revert-radar /tmp/demo --html /tmp/demo/report.html
```

## 怎么认定“回滚”

一个提交满足下面任一条就算回滚：

- 提交说明里有 `This reverts commit <sha>`（`git revert` 默认会写）
- 标题以 `Revert "`、`Revert:` 或 `Reapply "` 开头（GitHub 网页上的 Revert 按钮、新版 git 的回滚再回滚）

找原提交时先按 sha 匹配（支持短 sha），找不到再按引号里的原标题匹配。原提交本身也是回滚的，记为“重新合入”。合并提交不计入总数。

“被牵连的文件”取回滚提交本身改动的文件；如果回滚提交没有文件改动，就退回到原提交的文件列表。

## 限制

- 只认上面这些约定。手写“undo xxx”、或者用新提交手动改回去的，统计不到。
- 原提交不在扫描范围内（比如被 `--since` 截掉了）时，时间差算不出来，计入“找不到原提交”。
- squash 合并的仓库里，一个回滚可能对应一整个 PR，粒度会粗一些。
- 月份按运行这台机器的时区划分。

## 后续可以做

- 按目录聚合，而不只是按文件
- 对比两个时间段（比如引入 AI 编码工具前后）的回滚率
- 识别“修复上一个提交”的 fixup 链，作为回滚之外的另一个信号

## License

MIT
