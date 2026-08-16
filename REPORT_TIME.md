# REPORT_TIME — Unified Simulation Time Architecture

日期：2026-08-16
基线：`main @ f347755`（右侧边栏 UI）

基础架构升级：统一游戏内时间、倍率、系统节拍与长期时间表达。无新 Gameplay。

## Summary

- 新增权威 **SimClock**：唯一世界时钟，内部 **i64 微秒**，`T+HHH:MM:SS`
  累计显示（小时过 24/1000 不回卷、无游戏日）；
- 明确三层分离：Real Time（仅驱动调度/渲染/UI）、Simulation Time（一切
  Gameplay 依据）、Player Time Scale（Pause/1×/2×/4×，只改速率不改规则）；
- **固定步长调度**：Gameplay（电力/任务/扫描/移动）迁入 Bevy
  `FixedUpdate`，每步 = 1 ship-minute 固定 sim 步；玩家输入/动作系统保持
  逐帧（`Update`）。高倍率天然变为"每帧更多个固定步"，**绝不逐帧暴力重跑
  全部逻辑**；
- 全部 Gameplay 时长常量迁移到 sim 秒（×60），1× 玩家体感保持；
- Space 在 Pause 与**上次非零倍率**间切换；倍率表/标签单一来源；
  EventLog/UI 时间戳/冷却显示全部 Sim Time 化。

## Previous model

`Time<Virtual>` 直接当世界时钟：倍率直接改 `relative_speed`，Gameplay 读
`virtual.delta()`，所有常量以"现实秒"为隐式单位（1 virtual s = 1 gameplay s）。
无固定步长（帧率影响模拟），f32 elapsed 长期运行有精度风险，Space 行为是
Pause↔1×。

## New time model

| 概念 | 载体 | 用途 |
|---|---|---|
| Real Time | `Time<Real>` | 只在 `sim_pump_system` 里换算；渲染/UI |
| Simulation Time | `SimClock`（i64 µs） | 所有 Gameplay 的 now/dt/deadline |
| Player Scale | `GameSpeed`（索引 `SPEED_SCALES`） | 泵的乘数；Pause=0 |

核心公式：`advance = real_dt × BASE_SIM_RATE × scale`。

## Chosen temporary base rate

**`BASE_SIM_RATE = 60.0`**（1 real s @1× = 60 sim s = 1 ship-minute）。
理由：Fabricator 6 现实秒的周期自然成为"6 船分钟"，时长在船时里可读；
所有旧常量 ×60 迁移后 1× 体感不变。集中定义于 `simtime.rs`，随时可调。

## Duration migration

| 旧（现实秒） | 新（sim 秒 = 船时） |
|---|---|
| 拾取 0.3 / 卸货 0.25 | 18 / 15（PICKUP/DELIVER_SECS） |
| 重扫 0.2/0.3/0.5/0.6/1.0 | 12/18/30/36/60（RESCAN_*/SCAN_*） |
| 不可达冷却 15 | 900（15 船分钟） |
| 绕路 0.6 / 穿行 1.5 / 侧步 0.35 / 冷却 1.2 / 看门狗 2.5 | 36/90/21/72/150 |
| 建造 墙180 门180 架150 机480 堆600 缆90 / 拆除 90–240 | 同值×60 |
| Fabricator 周期 6 | 360（6 船分钟） |
| 船员速度 3 tiles/virtual-s | 3/60 tiles/sim-s（世界速度不变） |

## Simulation clock representation

`elapsed_us: i64`（1000 h = 3.6e12 µs，远小于 i64 上限）→ 整秒精确；
步推进只做整数减法，无 f32 累积误差。`backlog_us` 记录"已供给未执行"。

## Scheduler / cadence

- **Continuous（固定步长）**：`FixedUpdate` 每步 `SIM_STEP=1 sim s`，
  包含 Set::Jobs（power→task→scan）与 Set::Move。`sim_tick_system`
  在每步开头从 backlog 消费一步——**SimClock 只在真正执行的步里前进**，
  永不领先于世界状态；
- **Frame-based**：输入/相机/`actions_system`/UI/渲染留在 `Update`
  （动作事件每帧恰好消费一次；不受步数影响）；
- **Catch-up**：`Time<Virtual>::max_delta=250ms` 限制单帧现实增量
  （1×单帧最多 15 步、4× 60 步），剩余进 backlog 后续帧追赶——spiral of
  death 不可能发生；Debug 行显示 steps/frame、peak、backlog；
- **Scheduled/event-ready**：`SimClock.now()` 是绝对船时，未来
  `event at T+145:20:00` 即一个 f64 比较，架构不排除（本轮未实现）。

## Pause behavior

`scale=0` → 泵不供给 → `FixedUpdate` 不跑 → 时钟/移动/生产/施工/计时器
全部冻结；`Update`（UI/相机/选择/优先级/下蓝图/调速）照常。

## Speed behavior

`SPEED_SCALES=[0,1,2,4]` 单一来源（`simtime.rs`），UI 标签/按钮由它派生。
按键 1/2/3 设定并记忆 `last_nonzero`；**Space（TogglePause）** 在 Pause 与
last_nonzero 间切换；Pause 按钮在已暂停时亦恢复。

## HUD

顶栏新增 **`SHIP TIME T+HHH:MM:SS`**（与速度按钮构成时间区域）；右侧边栏
时间行同格式；事件日志 `[T+001:23:45]`；建造/操作剩余显示船时
（"6m left"）；冷却显示"retry in 15m"。Debug 展开行有 SIM 遥测。

## Determinism / equivalence results

- **倍率等价（tests/simtime::scale_equivalence）**：1×跑 2 现实秒 vs
  2×跑 1 现实秒 → sim=120.0 完全相等、船员位置完全相同；
- **FPS/节奏等价（fps_and_cadence_independence）**：60/30 FPS 与不规则帧
  （16/16/33/8/50/24ms 循环）→ sim 时间相等（不规则流允许 ≤1 步余数在
  accumulator 中，属设计行为）、位置与子格进度一致；
- **暂停等价**：暂停 30 帧后与不暂停直跑同 sim 时刻结果一致；
- **速度切换**：8 次连续切换后 clock == 实际执行步数（无重复/跳变）；
- **Hitch**：250ms 长帧 = 恰好 15 步、无积压残留；500ms 帧被 clamp。

## Long-duration precision

泵路径跑 100 现实秒（=6000 sim s）整秒精确；`format` 单测覆盖
T+024:00:00/001:40:00/1000:00:00/**T+10000:00:00**（i64 µs 无损）。

## Performance

1× = 60 步/现实秒、4× = 240 步/现实秒，每步为 4 船员 + ~30 实体的轻量
系统组；Debug 遥测实测（运行中）steps/frame ≈ 1–4、backlog ≈ 0。
高倍率未来路径：提高 scale 只增加步数（线性），或对子系统做 cadence 压缩
（架构已按"系统自选节奏"分层，不需要重跑全帧）。

## Automated acceptance

| 场景 | 结果 |
|---|---|
| A Clock baseline | ✓ 启动 T+000:00:00，1× 正常，HUD 显示（截图 208 teal px） |
| B 24h boundary | ✓ T+024:00:00→T+024:00:01 不回卷（单测） |
| C 100h+ formatting | ✓ T+1000/10000:00:00（单测） |
| D Pause | ✓ 时钟/船员冻结，UI 可用（集成测试） |
| E Pause resume prev speed | ✓ 4×/2× 两种（集成测试） |
| F 1×vs2× 等价 | ✓ sim/位置全等 |
| G 1×vs4× 等价 | ✓ 同上机制 |
| H FPS 独立 | ✓ 60/30/不规则 |
| I Irregular frames | ✓ ≤1 步 accumulator 余数 |
| J Movement | ✓ 等价性测试以真实 movement 系统断言位置 |
| K Production | ✓ SLICE0 I 场景 produced=3 回归 |
| L Construction | ✓ SLICE0 G/D/F 建造回归 |
| M Job retry/avoidance | ✓ path8 避让五测 + SLICE0 L 压力回归 |
| N Speed switching | ✓ 集成测试 |
| O Long frame | ✓ hitch 测试 |
| P Long-run precision | ✓ 6000 s 整秒 + 10000 h 格式 |
| Q High multiplier | ✓ 4×=240 步/s 由同一泵线性驱动（更高倍率同路径，未暴露 UI） |
| R Regression | ✓ 全部 91 测试绿 + SLICE0 A/D/F/I/M + SLICE2 A 场景重跑通过 |

## Playtest pass 1 — 普通 1×（SLICE0 A + M 对比）

M 布局 A/B：基线 5 件 62.6s → 65.3s、改造后 134.9（与 8-way 轮 62.6/135.2
一致，差异 <4% 属运行噪声）——**1× 体感保持**。A 场景 23 件入库、无卡死。
发现/修复：无（体感无突变）。

## Playtest pass 2 — 倍率切换

（simtime::rapid_speed_switching + pause_resumes_last_speed 覆盖核心语义；
场景 A/I 在 4× 下完成正常。）Space 现在恢复上次倍率而非回 1×。
发现/修复：初版 TogglePause action 没有被 speed 系统消费（测试暴露）——补上。

## Playtest pass 3 — 高负载时间推进

SLICE0 F（4× 加压 + 24 次入库）与 SLICE2 A 正常；Debug 遥测行确认无积压。
发现/修复：无。

## Design assumptions made

- BASE_SIM_RATE=60（临时，集中可调）；
- SIM_STEP=1 sim s（1 船分钟/步；60 步/现实秒@1×）；
- max_delta 250ms 作为隐式 per-frame 步预算（1×≤15 步、4×≤60 步/帧）；
- 不规则帧允许 sub-step 余数留在 accumulator（下一帧合并）；
- `T+HHH:MM:SS` 小时三位起、随位数自然增长；
- EventLog/边栏用完整 stamp，紧凑处（任务剩余）用 m/h 缩写。

## Temporary behaviors

- 玩家 UI 仍只有 Pause/1×/2×/4×（更高倍率仅架构就绪，无 UI）；
- 无 Cruise 时间压缩、无世界历法、无长期事件调度器、无存档；
- `Time<Virtual>` 仍存在但只作为 FixedUpdate 的 pacing 机制（倍率×BASE），
  Gameplay 不再读它。

## Known issues

- FixedUpdate 在 Update 之前执行：玩家动作最早下一帧生效（1 帧输入延迟，
  不可感知）；
- 极高倍率（64×+）时 movement+jobs 全量步进会线性变重——未来需按系统
  做 cadence 压缩（架构允许，未实现）；
- autotest 时间阈值沿用"旧 gameplay 秒"（sim/60）语义以保留历史调参——
  内部有注释说明。

## Git

branch `main`，commit 见 `git log -1`（"Unified simulation time: ..."），已推送 `origin/main`。
