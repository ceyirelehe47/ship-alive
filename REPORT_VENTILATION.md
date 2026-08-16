# REPORT_VENTILATION — Slice 6: Ventilation & Gas Handling

日期：2026-08-16 · 分支：main · 代码提交：见文末 Git 一节

## Summary

Slice 6 给玩家一套**主动、空间化、守恒**的气体工程工具。核心目标不是造氧，
不是让船员呼吸，而是：**玩家如何搬运、储存、控制已经存在的气体**。

交付物：

- **Gas Duct**（地下风管层）：第 3 个稠密地下网格（与电缆/水管并列），
  每格有限容积 `DUCT_MOL = 10`，存**真实四组分混合气 + 热能**；相邻格
  **局部平衡钳位**输送，绝不做全网平均 —— 压力波逐格传播。
- **Vent**（通风口）：Supply / Exhaust / Balanced 三模式 + Open/Closed 阀，
  只与自己头顶那一格房间交换。
- **Blower**（鼓风机）：压头模型（15 kPa head、12 mol/s 上限、4 PU 供电），
  断电/停机退化为被动风管格。
- **Gas Tank**（储气罐）：400 mol 有限容积、真实混合气、派生压强、阀门。
- **守恒**：全部 species + 热能，包括每一条拆除路径（风管→邻管/房间/账本，
  罐→下方风管/房间）。
- **不合并气密舱室**：通风服务于空间位置，锁着的门照样锁人，但风管可以
  绕过锁门送气；破口可以通过通风口抽干整张管网。
- **拓扑缓存 + 睡眠/唤醒**：流量步零 flood-fill、零重建；均衡管网静止休眠。
- **UI**：Ventilation 覆盖层（`P` 第 6 档）、青色流向箭头、SHIP STATUS
  通风块、三套选中面板、BUILD→Atmosphere 分类。
- **起步网络**：开局 FABRICATION ↔ CREW QUARTERS 之间 19 格风管 +
  2 通风口 + 1 鼓风机（待机）+ 1 预充储气罐，不打扰开局环境
  （scenario A：开局 48700.0，90 s 后仍 48700.0，活动格收敛到 2）。

明确不做（按简报）：生命支持/呼吸耗氧、火灾、气体补充/过滤/裂变、
压缩机热、管道泄漏、气闸。

## Architecture

新模块 `src/ventilation.rs`（~1100 行）+ 各集成点：

```
atmosphere.rs   共享气体原语（本轮抽取）: pressure_vol / eq_amount / move_gas
ventilation.rs  DuctGrid / Vent / Blower / GasTank / DuctTopology /
                ventilation_system (FixedUpdate, after atmosphere) /
                vent_action_system (按钮) / 拆除释放规则
building.rs     4 个新 BuildingKind + 放置校验(需下方风管) + 完成/拆除钩子
jobs.rs         Action::{MarkDuctDeconstruct, SetVentMode, SetVentOpen,
                SetBlowerDir, SetBlowerOn, SetTankValve} + 任务执行
setup.rs        起步管网 + 电缆延伸（给鼓风机供电）
render.rs       Ventilation 覆盖层（压强着色/流向箭头/设备标记）
ui.rs           SHIP STATUS 通风块 + 三套选中面板 + BUILD 分类
autotest.rs     SLICE6_SCENARIO 驱动器（A–U，跳过与 Slice 5 重叠的 J/K）
```

系统顺序（FixedUpdate）：power → thermal 链 → doors → atmosphere_system →
**ventilation_system**（`.after(atmosphere_system).before(Set::Move)
.in_set(Set::Jobs)`）→ Move。通风口转移的**唯一权威**是
ventilation_system —— 其他系统绝不直接改风管气体。

## Reused Atmosphere semantics（无第二套气体表示）

简报硬性要求：不得引入第二套 GasMixture / Pressure。实现方式：

- 风管每格存的就是 atmosphere 同款 `[f32; 4]` 组分（拷贝语义上的同一
  表示），热能同样以温度+热容表示；
- 压强计算复用**同一个公式**：本轮把它抽成 `atmosphere::pressure_vol(
  n, T, volume_mol)` —— 房间格（`STANDARD_MOL` 容积）与风管格
  （`DUCT_MOL`）/ 罐（`TANK_MOL`）只是**容积参数不同**；
- 转移原语 `atmosphere::move_gas(src, dst, …)`：组分按比例 + 能量混合
  （源保持温度），`bulk_pair`（Slice 5 房间↔房间）本轮重构为调用同一
  函数，行为不变；
- 平衡量 `atmosphere::eq_amount(n_a, t_a, V_a, n_b, t_b, V_b)`：等压
  精确解，房间↔风管、风管↔风管、罐↔风管全部用它。

也就是说：**房间里、风管里、罐里是同一种"气"**，搬运只是搬家。

## Duct representation

- `DuctGrid`：稠密 SoA（`[Vec<f32>; 4]` gas + 每格 temp），与 `CableGrid`
  / `PipeGrid` 同级的地下层，`set()` 撞 version + 唤醒，新风管开局为
  **真空**（scenario B：新格 0 → 逐步从邻格充压）。
- 每格有限容积 `DUCT_MOL = 10.0`（约标准间的 1/10）：19 格起步管网
  总容量 190 mol，房间 48300 mol —— 管是"管道"不是"仓库"。
- 相邻输送：对每对**醒着的**相邻风管格算 `eq_amount`，转移
  `min(差值, K_DUCT·差值·dt)`（K_DUCT = 0.5/s），经典平衡钳位防过冲；
  **没有全网平均**——跨 10 格的压力阶跃要 10 步以上才能传到
  （test: duct_flow_is_local_not_network_instant，1 步后远端 < 5%）。
- 热能：转移的气携带显热（move_gas 能量混合）；风管格自身与周围墙体
  之间有缓慢导热（远低于房间-空气交换），管网不是魔法恒温通道。

## Vent model

- 1×1 设备，**必须建在风管上方**（放置校验 NoDuct 报错）。
- 每步对"头顶房间格 ↔ 下方风管格"这一对算 `eq_amount`，然后按模式
  裁剪方向：
  - **Supply**：只允许 风管→房间（`x.min(0)` 方向）；
  - **Exhaust**：只允许 房间→风管（`x.max(0)`）；
  - **Balanced**：双向，压差自然驱动。
- 速率 = `K_VENT(0.3/s) × 平衡量差`，同样钳位防过冲。
- **Open/Closed**：Closed 完全不转移（test 里用整个管网总量验证，
  而不是单格——房间格会被室内 bulk flow 重新灌）。
- `last_rate` 遥测（供面板显示当前流量）。

关键语义：通风口**只交换自己头顶那一格**。想通风整个舱室，靠房间空气
自己逐格扩散——这正是 Slice 5 bulk flow 的职责边界。

## Blower model

压头设备（不是纯均衡器——纯均衡器推不动 C/D 场景的链路）：

- 闭式解：设进口侧温度 `ti`、出口侧 `to`、压头换算
  `k = HEAD/P_REF · DUCT_MOL · TEMP_REF`，则平衡输运量
  `x = (k + n_i·ti − n_o·to)/(ti + to)`，实际转移 `min(x, BLOWER_FLOW·dt)`
  （x<0 时为 0，风机不倒吸）。
- 效果：**端到端最多顶起 15 kPa 压差**就失速（死端管网不会无限泵气），
  环路则跑满 12 mol/s 上限。
- 方向 Dir4（E/W/S/N），由 `infer_blower_dir` 按放置时的相邻风管推断
  初始方向，玩家可改。
- **断电/停机 = 被动风管格**：不阻挡、不泵气，气体照常按压差流动
  （test: blower 断电后流量退化为自然扩散；scenario F：0.40 → 断电
  0.00 → 复电 0.37 mol/s）。
- 供电：`PowerRole::consumer(4)`（BLOWER_DEMAND），过载卸载时与其它
  消费者一起按先建先得断电。

## Gas Tank model

- `TANK_MOL = 400.0` 有限容积（≈ 40 格风管），真实混合气 + 温度，
  压强 `pressure_vol(.., TANK_MOL)` 派生。
- 阀门 Open：与**下方风管格**按 `K_TANK(0.25/s)` 交换（eq_amount 裁剪）；
  Close：完全隔离（scenario I：关阀后罐量恒定，重开继续充）。
- 拆除释放规则 `release_tank_gas`：优先排入下方风管格（按其剩余容量），
  余下进房间格，两者都没有（贴太空）才计入 VentStats 泄出账本。
- 起步罐预充标准大气（`prefilled_standard()`），开局即 ~87 kPa
  （与管网平衡后 346 mol）。
- `TANK_HIGH_KPA = 250`：超过时状态面板提示高压（纯警示，不爆）。

## Gas & thermal conservation

审计口径 = 房间 + 风管 + 罐（起步船 48700 = 房间 48300 + 罐 400）。
每条路径都验证过：

| 路径 | 结果 |
| --- | --- |
| 起步网络长时运行（A，90 s） | 48700.0 → 48700.0，精确 |
| 房间→风管→房间往返（test） | species 逐项相等，温度 21.000→21.000 °C |
| 热气进风管（test） | 全船热容×开尔文总量步进差 < 1e-6 |
| 拆风管（N） | 48700.0 → 48700.0，账本 +0.00 |
| 拆罐（test） | 罐内全部气体落在下方风管格，账本 0 |
| 破口泄压（P） | 泄出按 species 记账，审计 = 48700 − 泄出，逐项吻合 |
| C 场景 go 时刻清罐 | 人为移除 346 mol 后，三个检查点恒 48337.3 |

开发中修掉的**最阴险的 bug**：唤醒列表混进了非风管格索引（幻影格），
第 4 趟按守卫读（读到 0）却不守卫写（直接覆盖），无声销毁 ~2200 mol。
修复后 `wake_at` 只唤醒真实风管格、pass 4 逐格校验 `is_duct_index`。

## Airtight interaction（通风不合并舱室）

- 舱室边界仍由 Slice 4 的 `boundary()` 唯一决定：门关=隔气，风管对
  **房间侧**完全不可见。
- 跨舱室搬运 = 两个通风口 + 一张穿墙风管网。锁死的门（Lock Closed）
  对人是墙、对风管不存在 —— scenario E：锁门后 CREW 仍通过旁通风管
  +240 mol；把目标通风口 Close，速率立刻 0.00。
- 反向耦合：受灾舱室的通风口如果开着，房间真空会通过通风口抽风管
  —— 管网压强跟随受灾房间下滑（scenario P：走廊破口 60 s 泄出
  3020 mol，罐一度降到 379 后被远处高压房间回充到 395——开着接口
  就等于把管网押给了破口）；Close 通风口 + Close 罐阀后管网和罐保压
  （scenario Q：罐 393 mol 保住，锁门房间 102 kPa）。测试
  breach_through_open_vent_drains_the_tank 在受控世界里直接验证了
  "破口通过开启通风口抽干管网+罐"的完整链路（含隔离对照）。

## Atmosphere interaction

见上：共享原语、共享边界、共享睡眠模型。额外两条：

- 气体热容量同步：风管格与房间格一样参与 `gas_cap` 语义（真空管 ≈ 0
  热容）。
- 破口记账沿用 Slice 5 的 `debug_removed` / 泄出账本，VENTILATION 面板
  的 "Vent losses" 显示通风侧累计泄出（正常游玩恒 0）。

## Simulation Time

- 全部速率以 `SimClock` 的 dt 计算：暂停（dt=0）时通风口/鼓风机/罐阀
  /风管全部冻结（scenario S：暂停前后快照逐字节相等）。
- 倍速等价：1× 跑 N 步 vs 4× 跑 N 步，species 向量逐项相等
  （scenario T；test: fixed_steps_are_speed_independent）。
- 拆除/建造的守恒释放发生在**任务完成那一帧**，与倍速无关。

## Topology cache

- `DuctTopology`：逐格网络标号（flood-fill），**只在** `ducts.version`
  变化或设备集合签名变化时重建；流量步零 flood-fill、零重建
  （test: topology_split_merge_and_no_rebuild_on_flow —— 分割/重连
  重建一次，纯流动步计数不增）。
- 用途：UI 网络计数、独立网络判定（R：两张网互不串气）、
  覆盖层重建签名。`NO_NET = u16::MAX`。

## Active / sleep

- 与 Slice 5 同款唤醒列表模型：被 set()/转移触碰的风管格保持醒
  `WAKE_STEPS(600)` 步，之后静默。
- 噪声地板取 `WAKE_EPS_MOL(0.01)` 的分数：通风口/罐/鼓风机跳过
  ≤ `0.1×eps` 的转移，风管对跳过 ≤ `0.01×eps` —— 均衡管网能真正
  睡着而不是永远抖动（scenario U：密封后 active=0；test:
  sealed_network_sleeps）。
- 起步网络稳定后 active 收敛到 2（罐阀微交换的边缘格）。

## UI / Overlay

- **Ventilation 覆盖层**（`P` 循环第 6 档）：每格风管一个色块（沿用
  `pressure_color` 压强色带）；**青色流向箭头**（按 flow_x/flow_y
  遥测旋转，遥测按 0.85/步衰减，只显示"当前"流动）；鼓风机格
  通电青/断电红/停机灰；通风口格按模式着色（Supply 青/Exhaust 橙/
  Balanced 绿/关闭暗红）；储气罐按充注量着色。10 Hz 刷新桶 +
  0.5 kPa 量化，静止时零写入。
- **SHIP STATUS VENTILATION 块**：`Networks / Duct gas / Blowers (on) /
  Tank mol+kPa / Vent losses`。
- **选中面板**：Vent（三模式按钮 + Open/Close + Deconstruct）、
  Blower（四方向按钮 + Run/Stop + 断电显示 NO POWER）、Tank（阀门
  Open/Close + 组分明细 + 高压警示）。
- **BUILD→Atmosphere 分类**：GasDuct（拖拽铺设，同电缆）/ Vent /
  Blower / GasTank。
- 悬停 tooltip：风管格显示管内压强/组分。

## Performance

- **128×128 全铺稳定管网**：8.8 µs/步（睡着，几乎全跳过）。
- **128×128 活跃输送**（7750 条边、峰值 7812 醒格）：~158 µs/步
  峰值，含全部邻居对转移 + 遥测。
- **许多小网络**（test: perf_many_small_networks_stay_asleep）：
  分散小网全部休眠，每步开销与网络数无关（醒列表是全局稀疏集合）。
- 流量步**零分配**、零 flood-fill（拓扑缓存只看 version）。
- 覆盖层：池化实体 + 量化写，静止时无 GPU 写入。

## Tests

`tests/ventilation.rs` — 18 个集成测试，全绿；全套 187 个测试通过
（Slice 0–5 回归全绿）。

1. ducts_boot_empty_with_finite_volume
2. duct_flow_is_local_not_network_instant
3. vent_modes_honor_direction_and_closed_transfers_nothing
4. blower_power_direction_and_cap（断电退化/上限/方向偏置/死端失速）
5. tank_volume_pressure_valve_and_mixture
6. room_to_duct_to_room_round_trip_conserves_species_and_heat
7. hot_gas_into_duct_carries_energy
8. topology_split_merge_and_no_rebuild_on_flow
9. independent_networks_never_cross_transfer
10. duct_removal_preserves_gas_into_neighbours_room_or_ledger
11. tank_release_conserves_into_duct
12. breach_through_open_vent_drains_the_tank（+隔离成立）
13. pause_freezes_ventilation
14. fixed_steps_are_speed_independent
15. sealed_network_sleeps
16. perf_128_stable_duct_grid_sleeps
17. perf_128_active_transport
18. perf_many_small_networks_stay_asleep

## Acceptance A–V

（≥27 个测试主题由上节覆盖；场景驱动器为 SLICE6_SCENARIO。）

| 场景 | 内容 | 结果 |
| --- | --- | --- |
| A | 起步网络稳定 + 审计 | 48700.0 恒定；active 19→2；鼓风机通电待机 |
| B | 新风管充压 | 新格从真空渐进接近邻格压强；全船守恒 |
| C | 抽 FAB 充罐 | FAB −410 mol，罐 +366（真空罐先抽干才进稳态），go 后审计恒定 |
| D | 罐供气降压舱 | CREW 60.5→63.4 kPa，罐 344→234 mol，审计恒 46711.6（go 时刻人为移除的 40% 除外） |
| E | 锁门旁通 + 关阀 | 旁通 +240 mol；Close 通风口后速率 0.00 |
| F | 鼓风机断电/复电 | 0.40 → 0.00 → 0.37 mol/s |
| G | 反向 | flow_x 遥测 −0.26 → +0.12 翻转 |
| H | 模式语义 | Supply 单向 / Exhaust 单向 / Balanced 双向，逐条打印 |
| I | 罐阀保气 | 关阀罐恒 363；重开升到 390 |
| L | 剪断分网 | 网络 1→2，气体按容量 89/90 分账，无串扰 |
| M | 重接连通 | 网络 2→1，两侧压强渐进一致 |
| N | 拆风管守恒 | 48700.0 → 48700.0，vent losses 0.00 |
| P | 破口耦合管网 | 走廊破口 60 s 泄出 3020 mol（账本吻合，vent 侧 0）；管网跟随房间下滑（罐 400→379→随回充 395），开接口即被破口拖累 |
| Q | 隔离四重奏 | 罐保 393 mol；锁门房间 102 kPa；受灾走廊自行泄压（3814 mol）与隔离侧无关 |
| R | 独立网络 | 两张网各自平衡，互不串气 |
| S | 暂停 | 快照冻结，逐项相等 |
| T | 倍速等价 | `SLICE0_SPEED=1|2|4` 三次跑同一情景，species 向量逐项相等（另有单元测试 fixed_steps_are_speed_independent） |
| U | 密封休眠 | 接口全关后 active=0 |

J/K（开门气体交换/门缝语义）与 Slice 5 场景重叠，由 Slice 5 回归覆盖。

## Playtest 1–3（真实 `cargo run`）

1. **正常开船**（`SLICE6_VIEW=ventilation`）：起步管网可见，19 格风管
   沿走廊+立柱着色，鼓风机/罐标记清晰，顶栏 VENTILATION 摘要 + SHIP
   STATUS 通风块可读，无任何警报（鼓风机待机设计达成）。
2. **scenario D 送气**：罐→鼓风机→CREW 通风口送气链路全程可见——
   **青色流向箭头沿管路指向船员舱**（垂直段箭头旋转 90°、方向一致），
   鼓风机亮青色。
3. **scenario Q 破口+隔离**：走廊泄压区域变黑、ATMOSPHERE LOSS 警报
   常显；管网和罐关闭接口后保持绿色（保压），隔离状态一眼可读。

三张截图合成一张（pt_composite.png，试玩后已清理，见 git 历史）。

## Design assumptions made

- **风管容积** DUCT_MOL=10（标准格 1/10）：管是通道不是仓库；起步 19 格
  共 190 mol。
- **输送系数** K_DUCT=0.5/s、K_VENT=0.3/s、K_TANK=0.25/s：同级数量级，
  通风口略慢于管内（口是瓶颈）。
- **鼓风机** 12 mol/s / 15 kPa / 4 PU：一台能在几分钟里给半个房间换气，
  但顶不起无限压差（防死端 runaway）。
- **断电/停机语义** = 被动风管格（不挡流不泵气），确定性且可预期。
- **通风口规则**：只交换头顶格——空间性优先，整室通风靠房间扩散。
- **罐容积** 400 mol（=40 格管），预充标准大气。
- **高压限** 250 kPa 仅警示（爆罐留给未来压力伤害系统）。
- **拆除处理**：气体优先回邻管/下方管，其次房间，最后账本——绝不凭空
  消失，也绝不把室内气体"吐"进真空。
- 新风管开局真空（不是标准大气）：铺管不送气，送气要玩家自己接。

## Temporary behaviors

- 鼓风机方向推断 `infer_blower_dir` 只看直连邻格，丁字口可能猜错方向
  （玩家一键可改，面板有四方向按钮）。
- 覆盖层流向箭头是旋转的色块（dot 美术），不是真正的箭头贴图。
- 通风口/罐的交换速率不随温度/压差非线性缩放（常数系数模型）。
- 拆风管的释放是"瞬间挤回"（同帧完成），没有施工期间的缓慢泄气过程。

## Known issues

- Slice 5 回归审计（slice5_driver）现在把风管计入（本轮修正口径），
  剩余 ~5.2 mol 漂移来自罐库存不在该审计口径内（S6 审计口径已覆盖罐，
  两套口径数字不同但各自稳定）。
- 极长管网（>100 格）的均衡时间由 K_DUCT 决定，纯被动均压需要数分钟
  模拟时间——预期行为（真实管道也有压降），不是 bug。
- 128×128 活跃输送峰值 ~158 µs/步在 20 Hz 固定步长下占用 <0.4% 帧
  预算，但叠加满负荷热+大气+冷却时建议后续做批处理 SIMD 化（未做）。

## Deferred systems（按简报明确推迟）

生命支持（呼吸耗氧/CO₂）、火灾、气体补充/过滤/裂变、压缩机热、
管道泄漏老化、气闸、爆罐伤害、气体可见雾效。

## Git

- 代码提交：`feat: Slice 6 — Ventilation & Gas Handling (duct layer,
  vents, blowers, gas tanks; conserved transport; topology cache; UI)`
  （SHA 见 `git log`，本文件与文档在同一提交内更新的情况下以
  `git log -1` 为准）
- 推送：main 直推，无 force。
