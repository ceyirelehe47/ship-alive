# REPORT — Playable Slice 3: Thermal & Cooling（热能与冷却）

> 交付日期：2026-08-16 ｜ 基线：`b53d8f4`（Slice 统一仿真时间）
> 代码：`src/thermal.rs`（新）、`src/coolant.rs`（新）+ 集成改动 12 个文件
> 测试：**112 通过 / 0 失败**（新增 8 个热能集成测试 + 模块单测），
> clippy 0 警告，rustfmt 通过。

## 0. 一句话总结

热量现在是**真实、守恒、空间分布、可运输**的量：设备发热跟随负载 →
空气传播（穿墙极慢，船壳绝热）→ 热交换器吸进冷却液 → 水包沿真实拓扑环路
推进 → 散热器排向太空（唯一出口，硬上限）。断冷却 = 房间级危机在几分钟内
成形：Overheat 降额 → Critical 应急电力（保泵防死锁）→ 修复后自愈。
开局预装一条稳定环路，什么都不做也安全。

## 1. 架构边界（与规格逐条对应）

| 规格要求 | 实现 |
| --- | --- |
| 仿真时间是先决条件 | 全部热系统跑在 `FixedUpdate`、`SimClock.dt()` 驱动（1 ship-s 步长） |
| ECS 设备 / 密集网格热 | 设备=实体（`ThermalBody`/`ThermalState`/泵/换热器/散热器/水箱），热=逐格 `ThermalGrid`（空气温度 + 墙体温度数组），**绝无** one-tile-one-entity |
| 温度≠热量 | 每次交换显式移动热量 `Q`（`Q = cap·ΔT`），`conduct()` 带均衡钳制，任何 dt 不过冲 |
| 守恒，唯一散热口 | `ThermalStats.injected/radiated` 全程记账；测试断言 Δ存储 == 注入−排放；`radiator` 是唯一负项 |
| 热穿墙慢 | `K_AIR_SOLID 0.35` / `K_SOLID_SOLID 0.25`（船壳绝热），空气 `K_AIR_AIR 22` — 热点局部成形 |
| 反应堆热随负载 | `48 + 480×(served/rated)` H/s，derate 不虚增热 |
| 制造机发热 | 工作中 24 H/s；Overheat 工作速度 ×0.4，Critical 停（操作员留在原地，进度冻结） |
| 状态+滞迟 | Normal →(≥80°C) Overheat →(≥120°C) Critical；恢复 <65°C/<100°C；带内不抖动（测试钉死） |
| 降额真实影响电力 | thermal_state 每步写回 `PowerRole::Generator.output`：Normal 100 / Overheat 60 / Critical = 泵需求+4 |
| 防死锁应急电力 | Critical 时输出恰好覆盖本网所有泵（`emergency_output_for`）——甩载（先建先得）永远甩不到泵 |
| 冷却液独立地下层 | `PipeGrid` 与 `CableGrid` 平行、互不知晓，可同格共存 |
| 真实拓扑、有限水 | 每步 flood-fill 成网（切割/合并即时报），水=(水量,温度)包，总量守恒 |
| 泵耗电 | `PUMP_DEMAND 6`；断电 → `flow=0` 滞止，只剩本地包扩散 |
| 水包沿环路推进 | 每网 DFS 走廊（闭环连续），同步轮转：每格放出 min(水量,flow)，满环=位移流（不可压缩），绝不过容量 |
| 热交换器方向性 | `q = K·(Ta−max(Tw,30))`（>0 才吸）；水更热时反向给空气加热（无源、不制冷）；均衡钳制 |
| 散热器有限、非重置 | `dump = min(26·(Tw−15), 900)` H/s/台，15°C 旁通；贴船壳才有效（建造时校验，船壳永存） |
| 预装稳定环路 | H(14,17) K(15,17) Z(17,17) Z(19,17) W(21,17) + 9 管，80% 预充 |
| 覆盖层互斥 | `OverlayMode` 单资源：Off/Power/Thermal/Coolant（`P` 循环），重建签名=温度整数量化 |
| 蓝图建造/管线拖画 | 管线同电缆拖拽；设备需建在管上；拆管水回邻管/水箱（含水箱加成容量），满了诚实溢出并记账 |
| 性能 | 唤醒/休眠：注入或 >0.002K 交换唤醒，600 步静默休眠；均匀船≈零成本 |

**明确不做**（规格禁区，均未做）：大气/密闭、火灾、维护、主动制冷、
水经济（取水/净水）、相变、物品温度。

## 2. 数值与手感（调参记录）

初版常数（墙 420 H/K、K_AIR_SOLID 7）跑了无头仿真后发现两个问题：
墙体占全船 95% 热容 → 危机要几小时才显形；Overheat 降额自身形成稳定点 →
永远到不了 Critical。三处修正后得到现行手感：

- **船壳绝热**：墙容 80/60 H/K、气→墙 0.35、墙→墙 0.25。房间守住自己的热，
  FABRICATION（~80 格 ≈ 2 k H/K）成为危机舞台。
- **满载危机节奏**（528 H/s 注入）：cut 环路后 Overheat ≈ 10–12 min（ship time）、
  Critical ≈ 23 min；修复后 138 s 内恢复 Normal。待机负载（173 H/s）下断冷却是
  慢烧（约 40+ min 到 Overheat）——真实且给玩家反应时间。
- **核心受损热**：Overheat 附加 `620×severity` H/s（severity = 超出恢复带的程度，
  Critical 边缘满额、向恢复带衰减到 0）。这保证断冷却必然升级（不会卡在带内），
  同时修好的环路一定净降温（不会滞留 Overheat）。
- **散热器上限 900 H/s/台**：2 台 → 满载 528 H/s 有 3.4× 余量；危机高温时
  净降温 ~800 H/s，恢复分钟级。

实测锚点（无头，真实系统链）：

| 场景 | 结果 |
| --- | --- |
| 待机 90 ship-min | 核心最高 **34.9°C**，全程 Normal，水 134.5 不变，radiated>0 |
| 满载（74 人工负载）环路完好 30 min | 稳定 **48.2–52.5°C** |
| 满载 cut 管道 | Overheat ~700 s，**Critical t+1751 s**，应急电力保泵 ✓ |
| 修复管道 | **138 s 恢复 Normal**，output 回 100，环路 611 H/s 排热 |

## 3. 守恒证明

`starter_loop_is_stable_at_idle` 每 90 ship-min 断言：

```
Δ(空气热 + 墙体热 + 水热) == injected − radiated   （相对容差 1e-4 + 5H f32 漂移）
```

热量唯一的出口是散热器（`radiated_total` 由 coolant 系统独占写入）；
墙体/机器格转换保持**温度连续**（离散建造事件，见 thermal.rs 文档注释——
连续交换严格守恒，转换不产生温度跳变所以不可能当免费加热/制冷器用）。
水守恒：轮转只搬不造；拆管重分配；测试覆盖（空邻居零溢出 / 满邻居诚实溢出 /
网络分合总量不变）。

## 4. 覆盖层与 UI

- `P` / View 按钮循环 **Off → Power → Thermal → Coolant**（互斥单资源）。
- Thermal：逐格热图（`heat_color` 0→110°C 蓝→绿→黄→红）+ 设备热状态环；
  温度按整度量化进重建签名（慢漂移不触发 60fps 重建）。
- Coolant：管道点水温着色、透明度=充盈度；泵/换热器/散热器/水箱状态环。
- 顶栏摘要行随模式切换；**热警告常显**（REACTOR OVERHEAT/CRITICAL 红字）。
- 右侧边栏 SHIP STATUS 新增 **THERMAL** 块：核心温度/状态、最热房间、
  冷却网络数/水量/排热率。
- 选中面板：反应堆（核心温度/状态/Critical 提示）、泵（环路状态/流量）、
  换热器（吸热率/水温水量）、散热器（排放率）、水箱（蓄水/水温）。
- BUILD 栏新增 **Thermal** 分类（Pipe 拖拽 + 4 种设备），B 键循环同步扩展。

## 5. 验收场景（SLICE3_SCENARIO，全过）

| 场景 | 内容 | 结果 |
| --- | --- | --- |
| A | 4× 跑 90 ship-min 启动稳定性 | core 33.0°C Normal、水 134.5 恒定、radiated≈injected ✓ |
| B | 拆管→危机→修复 | t51 标记/t689 拆除（网络分裂）/t2600 修/t3296 复通；终态单网 14 格、flow 3.4、dump 299H/s 追账、core 38.7 恢复中 ✓ |
| C | 拆泵的电缆→滞止→重接 | 拆后 flow=0；重接后 pump Powered、flow 3.4 ✓ |
| E | 拆水箱旁管道 | 水 134.5→134.5，spilled=0.00 ✓ |
| F | 覆盖层循环 | Off→Power→Thermal（日志逐条）✓ |
| R | 4× 600s 全栈回归 | 热系统安静运行，无溢出无警告 ✓ |
| V | `SLICE3_VIEW_N` + 截图 | thermal/coolant 覆盖层目检 ✓（截图留档） |

回归：SLICE0 M/F、SLICE2 A/F/PW 全过（M produced=10；S2-F 7 制造机 + 泵
demand 106/served 86 确定性卸载——泵按先建先得保电）。

## 6. 测试清单（tests/thermal.rs，8 个）

1. `starter_loop_is_stable_at_idle` — 稳定 + 水/热双守恒
2. `full_load_cooling_failure_reaches_crisis_and_recovers` — 满载危机全级联 + 应急电力 + 修复恢复
3. `thermal_state_hysteresis_no_flicker` — 带内保持、两级滞迟
4. `pause_invariance_and_speed_equivalence` — 1s 步进与 dt=0 交错帧等价（速度不改规则）
5. `unpowered_pump_stagnates_loop` — 泵断电滞止、复电即循环
6. `network_split_and_merge_with_water_conservation` — 切割成双网、复接成单网、水不丢
7. `radiator_dump_is_capped_and_loop_transport_works` — 排热上限 + 满载运输
8. `perf_128x128_synth_loop` — 见下

模块内单测：传导均衡/防过冲、容量加权、注入、守恒、空气扩散+休眠、
睡眠不传导、墙慢于空气、转换温度连续、热色带；管道层 set/version、
拆管保水/溢出、水热求和、散热公式。

## 7. 性能记录

| 度量 | 数值 |
| --- | --- |
| 启动船热步成本 | 活跃格 ~30–80（反应堆房间+环路），唤醒/休眠自动 |
| 128×128 合成舰（开放式大厅，无房间分隔的最坏情况） | 1000 步 **0.78 s**（≈1280 步/s）；活跃 12137/16384（热羽在开放空间自由扩散） |
| 游戏需求 | 4× = 240 步/s → 余量 5×+（真实舰有绝热墙分隔，活跃集远小） |
| 水包 | 14 格环路，每步 O(管网) 轮转 |
| 覆盖层 | 温度整度量化签名，慢漂移零重建 |

已知取舍：128×128 开放大厅是病态拓扑（真船房间隔墙会困住热羽）；
如未来地图大幅扩张可再把 wake 门槛调粗或按房间分块。

## 8. 试玩记录（3 轮）

1. **正常运营**（M 场景 4× + 截图目检）：生产/搬运/建造全部照旧，
   THERMAL 块显示 core ~31°C Normal、环路 155 H/s 排热，零干扰零警告。
   1× 体感与 4× 一致（暂停不变性测试背书）。
2. **冷却故障闭环**（B 场景）：拆管 → 分裂 → 缓慢升温 → 修复 → 追账式排热
   （299 H/s）→ 自愈。应急电力保证修复路径始终存在。
3. **热容量管理**（满载无头 + R 场景）：2 散热器对满载 3.4× 余量，
   扩产空间明确；加换热器/散热器/水箱是可预期的下一步玩法。

## 9. 已知限制 / 下一步

- 满载危机的**玩家侧**节奏在待机负载下偏慢（真实），试玩反馈后可调
  `REACTOR_LOAD_HEAT`/severity 曲线。
- 环路轮转的走廊在**带支路的树**上存在小幅顺序传送（热量守恒不受影响）；
  纯环（游戏实际拓扑）精确。
- 预充 80%→62.5%（5/8）以换取拆管保水余量；想更满可调 `STARTER_FILL`。
- 未做：多流体、外部真空散热片方向性、热电联产（发电余热已在负载曲线中）。

## 10. 交付清单

- 新文件：`src/thermal.rs`、`src/coolant.rs`、`tests/thermal.rs`、本报告
- 集成：`map.rs`（p/K/W/Z/H 字符+SpawnReq）、`setup.rs`（预装环路+预充）、
  `building.rs`（5 种新建筑+地下管线+船壳校验+热组件）、`jobs.rs`
  （动作/调度链/施工完成/拆除保水/制造机热限速）、`power.rs`（OverlayMode 迁移）、
  `render.rs`（两套覆盖层+设备贴图）、`ui.rs`（Thermal 分类/THERMAL 块/
  选中面板/警告/View 按钮）、`input.rs`（管线拖画+拆管点选）、
  `autotest.rs`（SLICE3 场景 A/B/C/E/F/R/V）
- 测试基线 91 → **112**；场景 A/B/C/E/F/R 全过；SLICE0/SLICE2 回归全过
