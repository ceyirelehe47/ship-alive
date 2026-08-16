# AGENTS.md — Ship Alive 开发代理须知

给在本仓库工作的 AI 代理（Codex / ZCode 等）的经验与约定。

## 外部求助策略（按问题类型选工具）

### 逻辑性问题 → 优先咨询 Codex
算法设计、bug 根因链分析、架构取舍、并发/死锁推理等：

```bash
codex exec --skip-git-repo-check -C . "<问题，附上代码路径让 codex 自己读>。只读代码不要改。" < /dev/null
```

- 对话端点**不要**加 `-c model_provider=openai`（那是给生图用的；对话走默认路由即可）。
- 若报 `401 invalid_api_key`：执行 `codex login`（官方 ChatGPT OAuth，浏览器完成）后重试。
- 若报 `403 region not supported`：检查网络代理出口，或改用 ChatGPT 登录态。
- 可以让 codex 在后台跑（长问题），同时自己继续干别的。

### 时效性问题 → 优先网络搜索
API 签名、crate 版本行为、框架迁移差异、报错原文等随时会变的东西，
不要凭训练记忆作答，先搜（Bevy 0.16 的截图 API 就是这样查到的）。

### 两者都不是第一步 —— 先用项目自带诊断
遇到"船员卡住/行为异常"这类问题，先用现有日志定位，不要猜：

- `SLICE0_TRACE=1 SLICE0_SCENARIO=... cargo run` — 每 2s 打印船员轨迹/任务
- `SLICE0_SCAN_DEBUG=1 ...` — 每帧打印工作扫描/任务内部状态
- `SLICE0_SHOT=<frame>[:<path>] cargo run` — 引擎内截图（Bevy Screenshot API）
- 事件日志（EventLog）在游戏 HUD 与场景 dump 里都有输出

实际案例：侧步让位曾因重置计时导致两格乒乓死锁，是 TRACE 日志里
`pos=(19,15)↔(20,15) prog=0.07` 的重复帧暴露的，不是靠读代码看出来的。

## 本仓库踩过的坑（避免重蹈）

1. **多步 Python 补丁脚本中途断言失败 = 什么都没写入**。补丁要小步：
   一个脚本改一处、立即写盘、`assert old in s` 验证、再跑 `cargo check`。
2. **bash heredoc 在本机不可靠**（长脚本 / 特殊字符会截断）。
   复杂改写用 Write 工具写 `.py` 文件再 `python file.py` 执行，用完删除。
3. **rustfmt/格式化进程会和你竞争文件**：Edit 工具报 "File has been modified"
   时，重新 Read 再改；给锚文本做匹配时以**当前文件内容**为准，不要凭记忆。
4. **Bevy 0.16 `World::query::<T>().iter()` 只产出数据项**，Entity 必须显式
   放进查询元组：`query::<(Entity, &Foo)>()`。
5. **系统参数上限 16**：参数超了 `in_set` 会报莫名 trait 错误 — 拆系统或合并查询。
6. **同帧互斥预约**：Commands 是延迟的，同帧内多次领取用 local `HashSet`
   兜底（见 `jobs.rs` 的 `local_claims`）。
7. **移动系统只认 4 向邻接**：任何动态改 `path` 的逻辑（侧步/绕路）必须
   保证插入的格与前后格 4 向相邻，否则会产生对角瞬移。
8. **唤醒列表必须只装真实格索引**（Slice 6 幻影格事故）：`wake_around`
   曾把非风管坐标也推进唤醒列表，遍历时守卫读（越界→0）+ 不守卫写
   （直接覆盖）= 无声销毁 2200 mol 气体，守恒测试逐对全绿但总量掉。
   教训：稀疏活跃集的写路径和读路径必须同一套成员校验；总量审计
   （rooms+ducts+tanks）要作为常驻断言，不能只对单次转移做守恒。
9. **UI 遥测色不能在刷新趟里被换掉**：覆盖层实体池的 spawn 颜色会被
   周期刷新趟覆盖——改颜色要改刷新趟那一处（Slice 6 流向箭头一度
   被 10 Hz 刷新写成近白色，截图里"看不见箭头"才暴露）。
10. **`SLICE0_SPEED` 曾经每帧强制覆盖 `GameSpeed`**（`perf_report`）：
    任何验收 driver 想在运行中变速/恢复都会被它压回，进程表现为
    "永不退出的挂死"。现已改为只在首帧强制；写需要中途变速的场景时
    记得这个历史。
11. **墙不是 Building 实体**：船体 `#` 是地图 Tile，`MarkDeconstruct`
    只对实体建筑（架/门/反应堆/水箱…）有效。验收场景想拆"墙"时
    要选最远的 Building，别 filter `BuildingKind::Wall`（结果为空、
    场景静默跑偏）。

## 工程约定

- 质量门禁：`cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test` 全绿才算完成；不许删测试或大面积 `#[allow]` 压警告。
- 验收场景：`SLICE0_SCENARIO=A..L,P1,P2,M`（0/0B/1 代）、`SLICE2_SCENARIO=A..J`（电力代）、
  `SLICE6_SCENARIO=A..U`（通风代，跳过与 S5 重叠的 J/K）、`SLICE7_SCENARIO=A..F`
 （优先级代，F 配 `SLICE0_SPEED=0`）。改动核心系统后全量回归。
- 报告文化：每个 Slice 一份 `REPORT*.md`，含 Design assumptions / Temporary
  behaviors / 发现的问题；临时决定不许悄悄升级成永久设计。
- 美术管线：`art_raw/`（Codex 生图，洋红底）→ `cargo run --bin prep_art` →
  `assets/art/`。生图命令需要 `-c model_provider=openai`。
- Git：直推 `main`，禁止 force push，push 前 `git fetch` 处理并发改动。
