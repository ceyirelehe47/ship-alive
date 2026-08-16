# Ship Alive — Playable Slice 7 交付报告：工作优先级列表（WORK Tab）

> 目标（用户简报）：**"接下来做类似于环世界或缺氧的工作优先级列表"**。
> 交付：环世界（RimWorld）式的 WORK 面板 —— 三类工作 × 每名船员的
> 优先级矩阵，点击循环改档；配套两个模拟层语义修复（改档即时唤醒
> 空闲扫描、任务中改档绝不打断）；以及 Defaults 一键重置。

## Summary

- **WORK 面板**（`src/worktab.rs`，新模块）：`Tab` 键 / 顶栏 **Work [Tab]**
  按钮 / 船员检视面板 **Open WORK [Tab]** 按钮三处入口；`Esc` 先关面板。
  行 = 工作类型（Haul / Build / Operate，各带一行说明），列 = 船员
  （点击列头名字选中该船员，联动右下检视面板）。**Current 行**实时显示
  每名船员正在做什么（复用 `task_label`，含空闲原因），**Done 行**显示
  终身 h/b/o 计数（10 Hz 墙钟刷新）。
- **点击循环改档**：`—`(Off) → `L` → `N` → `H` → `—`。格子显示**当前**
  档位（色码：H 绿 / N 白 / L 灰 / — 红关，底色随档），点击写入
  `Action::SetPriority{level: current.cycle()}`。
- **即时唤醒**（`jobs.rs` `actions_system`）：`SetPriority` 落地时若该船员
  空闲，`next_scan = now` —— 不再等 nothing-to-do 退避（最长 60 sim s）。
  `ResetWorkPriorities`（Defaults 按钮）对全员同样处理。
- **绝不打断**：优先级只影响"下一个任务"的选择；进行中的任务不受影响
  （环世界语义）。
- 选中船员面板原来的 12 个优先级按钮移除（信息统一入口 WORK 面板），
  检视文本改为指向 WORK 面板。

## Architecture

| 位置 | 内容 |
| --- | --- |
| `src/crew.rs` | 模型扩展：`Priority::{cycle,code,color,bg}`、`WorkKind::desc` |
| `src/jobs.rs` | `Action::{ToggleWorkTab, ResetWorkPriorities}`；`SetPriority` 分支增加空闲唤醒；`ResetWorkPriorities` 处理（全员回默认 + 唤醒 + 日志） |
| `src/worktab.rs`（新） | `WorkTabVisible` / `WorkTabHud`（8 列池）/ `WorkTabButton` 标记；`build_work_tab`（Startup）、`work_tab_toggle_system`（消费开关动作 + 根可见性）、`work_tab_system`（签名重建 + 逐帧样式 + 点击选人 + 活动行刷新） |
| `src/ui.rs` | 顶栏 Work [Tab] 按钮（`WorkTabButton::Toggle`）；船员选中面板改为单入口按钮；`label`/色彩常量提为 `pub(crate)`；UiPlugin 注册新系统（与 hud_update/selection_panel 串行链，规避 BackgroundColor/Text 查询冲突） |
| `src/input.rs` | `Tab` → `ToggleWorkTab`；`Esc` 先关 WORK 面板再退工具/选择 |
| `src/autotest.rs` | `slice7_driver`（SLICE7_SCENARIO A–F） |

系统顺序：`work_tab_toggle_system` 在 Sync 集消费动作；`work_tab_system`
排在 `hud_update_system`/`selection_panel_system` 之后（链式，共享
`Text/TextColor/Visibility` 与按钮 `BackgroundColor` 查询）。面板可见性
由 toggle 系统单独驱动（`WorkTabRoot` 上的 `Visibility`），与文本池查询
无冲突。UI 全部墙钟节奏，暂停/倍速不影响面板交互（改优先级在暂停下也
生效——这是数据层动作，不推进模拟）。

## 优先级语义（复用 Slice 1 模型，未改扫描算法）

- 档位权重：Off 0 / Low 200 / Normal 500 / High 1000；候选打分
  `score = weight − min(dist, 60)`——**档位压倒距离**（High 的 60格外远任务
  仍然赢 Normal 的贴脸任务），档内才比距离。
- 领取互斥（`ReservedBy` + 帧内 `local_claims`）、四类任务执行、失败恢复
  全部沿用现状；本 slice 未动 `crew_scan_system` 的候选生成逻辑。
- 全员全关 → `IdleCause::AllWorkDisabled`；仅一类关且无其他候选 →
  NothingToDo（沿用）。

## 验收结果（SLICE7_SCENARIO，2026-08-17 实机）

| 场景 | 结果 |
| --- | --- |
| A 专职分工 | Ava=Haul-H/其余—、Rex=Build-H/其余—、Mio=Operate-H/其余—、Zed 全 N。快照与终局：`Ava[h=2,b=0,o=0] Rex[h=0,b=1,o=0] Mio[o=0] Zed[h=3,...]`，日志可见 Rex 只在蓝图送料齐后施工（"Rex started building Wall"），从未碰搬运；Mio 因制造机始终没凑齐 2 矿没活干（专职语义正确）。stored=19 reserved=2 |
| B 即时唤醒 | 全员 NothingToDo 后 MarkAll + Ava Haul=H：**Ava 1.0 sim s 后领取**（一个固定步，不等 60 sim s 退避）；t=30 时四人全在 haul、reserved=4 |
| C 高档压倒距离 | Ava(Haul=N) 贴脸有标记物品；标记距离 17.1 格的 Coolant Reservoir 拆除（Build=H）：`task=deconstruct`，其余三人（Haul 关）保持 idle |
| D 任务中禁用不打断 | Ava 搬运中 delivered=0 时关 Haul：完成手中的活（delivered 0→1 入架），之后 task=idle / "nothing to do"，不再领取新搬运 |
| E 全员停工 | 全员×全类 Disabled + MarkAll：四人全部 `Idle — all work types disabled`，reserved=0，marked_left=24 |
| F 暂停冻结+恢复 | `SLICE0_SPEED=0`：暂停下动作照常落地（prio_applied=High）但无人领取（全 idle）；driver 恢复 1× 后 t=1.0 四人全 haul、reserved=4 |

附带修复：`main.rs perf_report` 原本**每帧**强制 `SLICE0_SPEED`，会覆盖
运行中的变速（场景 F 无法恢复而挂死）——改为只在首帧强制一次。这是开发
工具行为修正，不影响正常游戏。

## 测试（193 = 187 + 6 新增，`tests/ship_ops.rs`）

1. `priority_cycle_visits_every_tier_in_order` —— 循环次序 N→H→—→L→N 与
   格子码 —/L/N/H 稳定。
2. `set_priority_wakes_an_idle_crew_instead_of_waiting_out_the_backoff` ——
   双船员对照：收到 SetPriority 的下一个固定步即领取；未收到的仍在退避
   （显式断言退避窗口 >20 sim s 存在）。
3. `disabling_work_mid_job_does_not_interrupt_the_running_job` —— 手中的
   搬运完成入库（stored=1, delivered=1），新标记物不再领取。
4. `all_work_types_disabled_reports_the_dedicated_idle_cause` ——
   AllWorkDisabled 且零预约。
5. `high_tier_beats_normal_tier_over_distance` —— 距离 1 的 Normal 搬运
   vs 距离 ~5 的 High 建造：选建造（此前只有 Low vs High 覆盖）。
6. `reset_priorities_action_restores_defaults_and_wakes_idlers` —— 全关
   状态下 Defaults：三档回 Normal 且下一步即领取。

## Playtests（实机 cargo run + 引擎内截图 + 视觉模型核验）

1. **PT1 默认视图**（`SLICE7_VIEW=work`）：面板居中上部，四列
   Ava/Rex/Mio/Zed 全 N，Current 全 "Idle — nothing to do"，Defaults /
   Close [Tab] 按钮齐全，顶栏 Work [Tab] 高亮。视觉核验通过。
2. **PT2 专才运行中**（`SLICE7_SCENARIO=A` + `SLICE7_VIEW=work`，帧 700）：
   矩阵 H/—/— ｜ —/H/— ｜ —/—/H ｜ N/N/N 与脚本一致（绿=H、暗红=—、
   白=N 色块分明）；Current 行 Ava "Carrying Machinery Part…"、Zed
   "Carrying Cargo Crate…"、Rex/Mio idle，与事件日志吻合；Done 行 0/0/0
   （该时刻确实尚无交付）。视觉核验通过。
3. **PT3 回归批**（S0-A/S0-K/S2-A/S3-A/S4-A/S5-A/S6-A/S5-O 全套重跑）：
   全部按预期完成（详见下方 Git 前的运行记录；S0-K 直接压测
   SetPriority 端到端路径）。

## Design assumptions

- **选择环世界（每船员×每工作类型）而非缺氧（每任务/每建筑）**：项目
  Slice 1 已有每船员 `WorkPriorities` 模型与"档位压倒距离"扫描，缺的是
  玩家可用的全局视图；缺氧轴（每蓝图/订单子优先级）记入 Deferred。
- 档位显示为字母码（H/N/L/—）+ 颜色，而非环世界数字 1–4：与现有
  Off/Low/Normal/High 语义一一对应，测试与日志零改名。
- 循环方向 `—→L→N→H→—`：默认 N 下连点两下即"专才+关"最短路径。
- 列头点击 = 选中船员（复用 Selection），不新增"选中列"概念。
- 船员列池上限 8（开局 4 人；超出静默截断——见 Known issues）。

## Temporary behaviors

- WORK 面板打开时不暂停游戏（环世界同样如此）；面板覆盖区域拦截地图
  点击（Interaction 拦截），拖框选不会穿过面板。
- `SLICE7_VIEW=work` 仅是启动即开面板的开发钩子（截图/验收用）。
- Current 行文本 26 字符截断（"…"）保持列对齐。

## Known issues

- 超过 8 名船员时 WORK 面板静默截断（当前游戏没有增员途径；将来加人
  时应改为动态重建列池）。
- Mio 在场景 A 全程无操作机会属于供应链节奏（Auto 供料排队在 Ava 身上），
  不是优先级缺陷；专职语义本身由 B/C/D/E/F 与单测钉死。
- 改档唤醒只作用于**空闲**船员；正在走路的船员到达当前任务终点前不会
  重估（环世界一致，不算 bug）。

## Deferred（本 slice 不做）

- 缺氧轴：每蓝图/拆除标记/制造订单的子优先级（1–9）与"紧急（!）"档。
- 船员个体差异（技能/热情影响工作速度——当前无技能系统）。
- 工作类型细分（Construct/Demolish 拆分、未来的 Research/Clean 等）。
- 优先级预设档案（"搬运队/工程队"一键套用）。

## Git

- 代码与文档提交：见仓库 main 分支最新提交（SHA 在下方回填）。
- 门禁：`cargo fmt` ✅、`cargo clippy --all-targets --all-features -- -D warnings` ✅、
  `cargo test` **193/193** ✅。
