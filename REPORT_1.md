# REPORT_1 — Playable Slice 1: Ship Operations

日期：2026-08-16
基线：`main @ db63aba`（Slice 0B）

## Implementation completed

### A. 舰内建造 / 拆除 / 改造

- **可建对象**（BUILD 栏，全部有 ghost 预览与非法位置红色反馈）：
  - `Wall`（1×1，1 Part，3s）— 完成后该格变为 `Tile::BuiltWall`，阻挡寻路；
  - `Door`（1×1，2 Part，3s）— 可通行，未来气密的挂点；
  - `Storage Rack`（1×1，1 Part，2.5s）— 完成即成为可用货架（带过滤）；
  - `Fabricator`（2×2，4 Part，8s）— 多格阻挡建筑，完成时把站在足迹上的船员
    挪到最近可走格。
- **建造成立的真实流程**：放置蓝图 → 自动物流从最近的**未标记地面零件**或
  **货架库存**调零件（inbound 计数防止超运）→ 材料齐 → 一名船员走到工地施工 →
  完成。超额送到的材料在完成时如数退还（材料守恒）。
- **取消蓝图**：退还已送材料、取消相关搬运/施工任务、清理预约。
- **拆除**：Deconstruct 工具或选中面板按钮标记 → 船员执行 → **全额返还建材**
  （货架同时掉落内存货物）→ 地格还原 Floor，寻路恢复。
  Hull 墙不可拆（无实体）；初始货架与初始 Fabricator 也可拆（鼓励重排）。
- **放置规则**：仅限 Floor（ hull 墙 / 已建墙 / 机器格上不可放）、不与建筑/蓝图
  足迹重叠、Wall/Fabricator 要求落点无地面物品（Door/Rack 允许）。

### B. 最小生产链

- 配方：**2 Asteroid Ore → 1 Machinery Part，6 秒/批**（唯一配方）。
- 订单：`+1 batch` / `+5` / `Repeat`（无限）/ `Clear order`。
- 空间化全流程：订单 → 自动物流从地面/货架拉矿入机器输入缓冲（上限 8）→
  输入满足 → 一名船员走到机器旁**实地操作 6 秒**（离开即中断且不耗料）→
  产出进入输出缓冲（上限 6）→ 自动物流把产出搬到接收零件的货架。
- 状态机（UI + 机器光环 + 头顶文字三处可见）：
  `no order / waiting for input / waiting for worker / working N% / output blocked`。
- 取消订单：中断循环、不消耗已入缓冲的矿。

### C. 仓储与物流深化

- **货架物品过滤**：选中货架，面板 `allow/deny` 按钮切换 Crate/Ore/Part；
  标签显示 `n/4 过滤摘要`（如 `4/4 Ore`）。
- **自动物流**（第一层，全部走真实空间搬运）：
  - 蓝图缺料 → 自动生成供料需求；
  - 机器缺输入 → 自动供矿（地面未标记物品优先，货架库存 +1 距离惩罚）；
  - 机器有产出 → 自动入库（只去接收该类别的货架）。
- 手动 Mark for Haul / 框选 / Haul All 全部保留；玩家标记的物品不会被自动需求
  挪用。
- 从货架拉料 = 立即从库存计数取出并在货架格生成实体物品 → 船员实走实搬。

### D. 多工作类型与最小优先级

- 三类工作：**Haul / Build（含拆除）/ Operate**。
- 每名船员对三类工作各有 **Off / Low / Normal / High**（选中船员的面板 12 键）。
- 领取算法：候选 = 所有可领取工作（含自动物流需求），评分
  `优先级权重(High 1000 / Normal 500 / Low 200) − min(距离, 60)`，
  同档就近；Disabled 永不领取；跨档严格优先。四名船员同帧串行领取 +
  in-frame 本地预约集合 + `ReservedBy` 组件，杜绝重复领取。
- 预约互斥覆盖：物品（搬运）、蓝图（施工）、建筑（拆除）、机器（操作）。

### UI

- **BUILD 栏**：Wall/Door/Rack/Fabricator/Deconstruct + Cancel Tool，当前工具高亮，
  提示行显示当前工具与操作方法；`B` 键循环切换，`Esc` 退出工具。
- **Ghost**：跟随鼠标的放置预览，绿=可放 / 红=不可放（附原因文字），Deconstruct
  工具高亮悬停建筑。
- **动态选择面板**：按选中对象显示 —— 蓝图（材料进度 + 取消按钮）、建筑
  （拆除按钮 + 拆除进度）、货架（库存 + 过滤开关 + 拆除）、Fabricator
  （状态/输入/输出/订单按钮 + 进度）、船员（当前任务 + 优先级 12 键）、
  物品（标记状态 + [T]）。面板按钮为固定槽位复用（改 label/action/可见性），
  不重建实体、不丢交互态。
- **反馈**：建造没开始 = 蓝图面板显示还缺什么；机器停工原因 = 状态文字 +
  光环颜色；船员闲着 = 底部状态条原因；全部无需 debug log。
- 顶栏新增 `Parts made / Built` 计数；HUD/事件日志/悬停 tooltip 均扩展到新实体。

### 其他改动

- **对头死锁修复**（0B 已知问题的放大版）：1 格走廊两人对向互堵时，原
  `blocked_for` 会被"偶尔未被堵"帧的 2× 衰减侵蚀，永远到不了穿行阈值。
  新增按**目标格**累计且不衰减的 `blocked_on_tile`，同一格被堵 1.5 秒必触发
  穿行。Slice 1 的生产供应链路因此不再会被走廊顶死。
- `Tile` 枚举扩展 `BuiltWall / Door / Machine`，完成/拆除即写回网格，寻路
  立即反映（有专项测试）。
- 新增 `stats.rs` 开发遥测（产量/建造数/搬运次数/搬运距离）用于布局 A/B。
- 新增引擎内截图（`SLICE0_SHOT=<frame>[:path]`，Bevy Screenshot API）用于
  无头视觉验证；外部窗口抓屏脚本另存为 `tools/capture_screen.ps1`。

## Existing systems preserved

Slice 0B 全部保留并回归通过：点击选择 / 悬停 tooltip / 相机平移缩放 /
手动搬运与框选 / Reservation / 失败恢复 / 货架仓储 / Pause-1×-2×-4× /
4 名船员 / Debug 工具（默认折叠）/ 事件日志 / 原 10 个 haul_logic 集成测试。
开局改为新布局（见下），旧场景 A–F 语义不变且全部通过。

## Automated tests

- **总数 44**（0B 为 19）：
  - 单元测试 17（storage 容量/过滤/库存取出、production 状态机 7 项、map 解析等）；
  - `tests/haul_logic.rs` 10（原测试，适配 phase 改名与链式调度）；
  - `tests/ship_ops.rs` 17（新增）：蓝图供给-施工端到端、非法放置、材料/施工/
    操作预约互斥、双份领取不重复、取消蓝图退款、墙阻断寻路 + 拆除恢复、
    生产端到端、无单不生产、产出阻塞停机、清订单保料、过滤分流、货架库存
    自动供料、Disabled 不领取、High 压 Low、混合工作无冲突、拆货架退建材+存货。
- 质量：`cargo fmt --check` ✓，`cargo clippy --all-targets --all-features
  -- -D warnings` ✓，`cargo test` ✓（44/44）。

## Acceptance scenarios

- **A–F**（0B 回归）：全部通过。A：24 件地面物入库 23（密封舱 1 件除外）；
  B：满仓静默；C：不可达红环；D：四人四目标；E：删目标安全恢复；F：4× 加压稳定。
- **G 建造** ✓：rack 蓝图 → 零件搬运（62 格）→ 施工完成（built_at_target=true）。
- **H 拆除** ✓：初始货架拆除、racks_left=9、材料返还、预约清理。
- **I 生产** ✓：订单 5 批，产出 ≥3 后采样：矿自动入机、船员操作、零件入库。
- **J 过滤** ✓：ore 架只得 ore（4），part 架只得 part，互不污染。
- **K 分工** ✓：hauler-only / builder-only / operator-only / 全能四人，计数器
  显示各自只做本职工种（如 Rex 只 build、Zed operate、Ava 搬运供料）。
- **L 4× 压力** ✓：3 蓝图 + 订单 + 大量搬运并行，built=3 produced=3，
  stuck_idle=0，无重复预约、无卡死、无资源丢失。

## Playtest performed

以"正常玩家动作"（即 UI 按钮所触发的同一套 Action）脚本化完成三轮：

1. **P1 建造流程**：建 Rack → 建墙（中途取消、材料退还）→ 建 Door →
   拆掉 Rack → 原地重建，`rack_rebuilt=true wall_canceled=true door_built=true`。
   过程可读：蓝图材料计数、施工百分比、拆除黄标都可见。
2. **P2 生产配置**：把 P/O 货架配置为 parts-only / ore-only（deny crate）→
   下单 3 批 → 观察供矿、操作、产出入库；`P2_FILTER_CLEAN` 两个方向均为 0
   （无跨类污染），机器 Working，2+ 件产出。
3. **M 布局优化**（见下节）。

另有引擎内截图验证（`SLICE0_SHOT`）：房间/墙体/彩色船员/物品/HUD/BUILD 栏
均正常渲染（窗口外部抓屏对 GPU 交换链不可靠，已改用 Bevy Screenshot API）。

## Layout optimization result

场景 M（4× 速度，遥测：`stats`）：

- **改造前（差布局）**：矿石架在船右下 STORAGE（离 FABRICATION 的机器约
  15+ 格），Repeat 生产，**产出 5 件用 70.6 秒**，期间搬运距离 260 格。
- **玩家改造动作**：在 Fabricator 旁建两个 Rack（材料真实搬运 + 施工）→
  设为只收 Ore → 其他所有货架 deny Ore → 拆掉远处两个 Ore 架（8 矿返还）→
  框选矿石区重新入库（矿只能进新架）→ 继续生产。
- **改造后**：**产出 5 件用 48.1 秒（吞吐 +47%）**；供料搬运从"跨船长途"
  变为"贴身短驳"。
- 复跑一次结果一致（70.6s → 47.5s，+48%），改造收益可复现。
- 结论：**"布局本身就是运营系统"在当前切片已可感知** —— 玩家有明确动机
  和手段去重构舰内空间并获得可见收益。

## Usability problems found and fixed

- 走廊对头死锁导致生产断料（见上）→ 按格累计阻塞时间修复。
- 扫描系统 fabs 查询误带 `Without<Building>`，制造机永不被人操作 → 修正。
- 机器产出物品缺 MarkedForHaul 被校验误杀 → 产出自动进入入库流程。
- K 场景 MarkAll 抢占专职工人运力 → 改为定向供料演示。
- `QueryState::iter` 单元素组不含 Entity 的 API 差异导致测试反复失败 →
  查询统一带 Entity 元组。

## Remaining usability problems

- **1 格门/走廊拥堵仍在**（0B 已知）：穿行机制保证不死锁，但高峰期船员会
  明显排队、偶有重叠视觉；生产越忙越明显。
- 自动物流只在"领取时刻"就近，不会全局优化；供料员与入库员可能反复长距离
  对穿。
- 蓝图没有"拖动连放"（墙要一格一格点）；没有框选多建筑批量拆除。
- Fabricator 无朝向概念；输入/输出共用缓冲区。
- 面板文字全 ASCII（默认字体无 CJK），Priority 按钮共 12 个略密。
- 4× 下多船员同时在机器旁操作时，环/文字会闪（每帧重算 label）。

## Design assumptions made（临时玩法决定）

- 拆除**全额返还**建材（含初始设施），鼓励布局实验；属 provisional design。
- 建材统一为 Machinery Part（Wall 1 / Door 2 / Rack 1 / Fabricator 4），
  生产链因此自闭环：矿 → 零件 → 更多建筑/机器。
- 门不要求贴墙放置、默认常开可通行；无气密/权限。
- 机器输入/输出缓冲上限 8 / 6；产出自动标记入库；玩家标记物品不被自动需求
  挪用；从货架取料在领取瞬间出库并在货架格生成实体。
- 领取评分为"档位压制 + 同档就近"，不做全局最优；High 档可以饿死低档
  （玩家配置使然）。
- 多格设备只做了 2×2 Fabricator（验证足迹/阻挡/邻接交互），无朝向。
- 物品仍是单实体（无堆叠 slot/stack）；货架容量 4/格 维持 0B 语义。
- 开局资源：地面零件 9 + 零件架 8 + 矿 16（地面 8 + 矿架 8）+ 货箱 6，
  足够多种布局试验与连续生产。
- Starter Ship 重排为六个房间（新增 FABRICATION 预置一台 Fabricator），
  尺寸不变（36×19）；密封舱移到左下做不可达回归。

## Temporary behaviors

- ASCII-only UI 文案（默认字体限制）。
- 拆除返还/初始设施可拆带来的"免费材料"仅限本切片经济观感。
- `SLICE0_SCAN_DEBUG` / `SLICE0_TRACE` 等诊断打印为开发工具，默认关闭。
- 场景 K/M 中矿源/补矿用 debug spawn 模拟"玩家继续采矿"。

## Design problems discovered

1. **实体货架在自动物流下成立**，但"从货架取料"瞬间出库会让库存数字
   短暂与视觉不一致（物品躺在货架格上待拾取）——可接受，但值得观察。
2. 货架过滤好用，但**没有过滤模板/复制**，逐架点选在大改布局时繁琐。
3. 自动物流确实消除了微操；玩家剩下的调节手段是**布局 + 过滤 + 优先级**，
   三者耦合度合适。
4. Blueprint + 搬材料 + 施工节奏偏慢（Supply → build 两段都要人走）；
   多蓝图并行时会感觉"建造工永远在路上"。
5. 一格门拥堵在 4 名船员 + 生产并行时**已成为真实瓶颈**（修复后不死锁但慢）。
6. 船员避让仍是无优先级的软避让；穿行视觉效果偶尔重叠。
7. Fabricator IO 无方向导致"原料口/成品口"分离的布局玩法暂不可做。
8. 多格设备（2×2）实现成本可控，交互（放置校验/邻接作业/拆除返还）都
   顺带验证了，后续可放心多格化。
9. 36×19 对 4 人 + 一台机器 + 改造空间**勉强够但不宽裕**，尤其走廊。
10. "玩家能否通过布局改善吞吐" —— **M 场景已给出 +47% 的正结果**。
11. 全额退款让"拆了重摆"毫无心理负担，实验频率显著提高（正反馈）。
12. 材料真实搬运在慢速下有乐趣（看得见物流），在 4× 忙时略拖节奏；
    远期或需"施工缓存箱"类缓冲。

## Technical decisions（实现选择，非设计规则）

- 工作仍无独立 job board：`MarkedForHaul`/蓝图缺料/订单缺输入即需求，
  `ReservedBy` 即预约，`CrewTask` 即执行态 —— 单一事实来源。
- `CrewTask` 扩为 `Idle | Haul | Build | Deconstruct | Operate`；
  `HaulJob.dest` 泛化 `Storage | Blueprint(e) | Machine(e)`。
- 建筑 = ECS 实体（`Building + Footprint + TilePos`），完成/拆除时写回
  `ShipMap` 网格格（`BuiltWall/Door/Machine`）驱动寻路与渲染分层。
- 交互格 = 足迹内可走格 ∪ 四邻域；多格机器天然支持"站在旁边作业"。
- 领取互斥：查询顺序串行 + `local_claims`（同帧）+ `ReservedBy`（跨帧）。
- 生产状态由 `Fabricator` 现场推导（`state()`），无独立 tick 系统；
  操作计时存于船员 `WorkJob`，机器 `progress` 供 UI/渲染。
- 选择面板按钮 = 固定 16 槽复用（`OnPress`/`BtnLabel` 组件改写），
  避免 UI 重建导致交互丢失。
- `stats.rs` 遥测挂在领取/完成事件上，场景 dump 与顶栏共用。
- 新场景驱动沿用 `SLICE0_SCENARIO` 环境变量（G–L + P1/P2/M）。

## Art assets added or changed

- 新增（Codex image generation → `prep_art` → 256×256 透明 PNG）：
  `door.png`、`fabricator.png`、`wall_built.png`（内部墙与船体墙区分）。
- `part.png` 因管线重跑被重编码（视觉等价）。
- 缺失资产清单：蓝图脚手架/施工标记、拆除叉标记、UI 图标（BUILD 栏目前
  用文字按钮）、产出/输入口贴图 —— 均以程序化 tint/文字代替，不影响可读性。

## Recommended design questions（给下一轮评审）

1. 拆除返还比例是否长期保留 100%？是否区分"玩家建/预置"设施？
2. 走廊拥堵：加宽开局通道、双格门，还是接受排队感？
3. 优先级按人配置 vs 全局工作档位，哪种更符合目标玩家的心智？
4. Fabricator 是否要输入/输出口方向化，以解锁更精细的布局玩法？
5. 货架是否引入 stack/slot 容量语义（当前 4 件/格）？
6. 自动物流是否需要"供料小车/无人机"雏形来缓解人力搬运瓶颈？
7. 施工是否允许"材料缓存箱"以平滑 Supply→build 两段等待？
8. 物品堆叠与搬运承重何时引入（影响仓储与人数平衡）？

## Recommended Slice 2 preparation（仅建议）

- 数据准备：工作/搬运遥测已就绪（`stats.rs` + 场景 dump），建议 Slice 2
  加"每 salvaged 吨位的搬运距离/工时"口径。
- 地图边界：`ShipMap` 的布局驱动生成已验证多房间结构，废船/对接舱段可以
  复用同一网格 + 蓝图系统（对接后把两张网格缝合，或同一张更大网格分段）。
- 建议优先做的地基：物品堆叠（如果要）、门朝向/状态机（如果硬对接要气密）、
  以及把 `IdleCause` 扩展为统一的"为什么没事做"报告口径。
- 美术：脚手架/拆除标记/UI 图标可在 Slice 2 前补齐。

## Branch / commit

- branch：`main`
- commit：见 `git log -1`（提交信息 "Playable Slice 1 — Ship Operations: ..."）
- push：已推送到 `origin/main`（github.com/ceyirelehe47/ship-alive）
