# REPORT_2 — Playable Slice 2: Ship Power

日期：2026-08-16
基线：`main @ 439f51b`（Slice 1 — Ship Operations）

## Summary

舰内电力第一次成为真实的空间系统：

- **地下电缆层**（`CableGrid`）：密集网格数据、不占地面、可在任何非船壳格下铺设（含内墙/机器/门下）；
- **真实拓扑**：发电设备经连续电缆连接到设备"电力接口"（足迹∪四邻域）才供电；切线即局部失电，重接即恢复；一船可同时存在多个独立电网；设备接口同时触及两个电缆组时电气上并网；
- **Starter Reactor**：2×2 真实地图设备（开局预置于 FABRICATION），有限输出 100 PU，可 Online/Standby、可建可拆；
- **Fabricator 成为电力消费者**（20 PU）：断电不生产、中断不耗料、复电安全恢复；
- **每个电网计算 generation / demand / served / headroom / deficit**，过载按 **Entity 顺序确定性卸载**（老设备保电，无每帧随机）；
- **Power Overlay**（`P` 键 / 顶栏按钮）：按网络着色的电缆、设备供电状态环、HUD 网络摘要行；普通视图断电设备也有 NO POWER 文字与暗环反馈；
- **建造/拆除接入现有 Construction**：Power Cable 是 0 材料蓝图（1.5s 施工），拆除走同一 Deconstruct 流（产生瞬时格实体）。

同时完成了两件份外但必要的修复：

1. **船员避让重写**（对头优先分流 + 侧步让位 + 单调看门狗），消灭了 Slice 1 遗留的走廊对堵与两格乒乓死锁；
2. **ring/dot UI 贴图改为程序化合成**（原 AI 生成 raw 是纯色洋红方块，处理后的"圆环"是实心板，完全遮挡被选物品 — 玩家实测发现）。

## Player-facing behavior

普通玩家现在可以：

- 按 `P`（或顶栏 Power 按钮）开关**电力视图**：电缆按网络着色（有源网络彩色、无源网络暗红）、设备周围供电状态环（绿=有电 / 红=无源 / 黄=过载卸载 / 灰=未接线）、顶栏网络摘要 `POWER | NET 1: gen 100 dem 20 served 20 — headroom 80`；
- 选中 **Reactor**：面板显示输出/状态/所在网络 gen/demand/summary，`Online / Standby` 按钮即时启停；
- 选中 **Fabricator**：面板多一行 `POWER: no cable — machine halted` 之类的停机原因；
- 用 BUILD 栏 **Power Cable** 工具**按住左键拖画**整条线路（绿/红 ghost 实时反馈），Deconstruct 工具点击电缆格标记拆除（黄框高亮）；
- 观察"发电机 → 地下线路 → 设备接口 → 设备运行"的完整因果链，并通过剪断/重接实验它。

## Power model implemented

- 单位：抽象 Power Unit（PU）。Starter Reactor 输出 100 PU；Fabricator 常载 20 PU。
- **网络 = 电缆 4 向连通域**；设备经"电力接口"（足迹格 ∪ 足迹四邻域）挂接到触及的第一个电缆域；**同一设备触及多个域时把它们电气并网**（union-find）。
- 每帧全量重算（flood-fill ~700 格，微不足道），拓扑永不陈旧 —— 新建/拆除/启停/分割/合并都即时生效，无需重启或手动刷新。
- 状态机：设备 `Unconnected`（接口无电缆）/ `NoGenerator`（网络无在线发电，含全部 standby）/ `Shed`（网络有源但容量不足且轮到它被卸载）/ `Powered`。
- **过载策略（临时，Design assumption）**：按 Entity id 升序服务消费者 —— 等价于"先建先得"，老设备保电、最新设备先被卸载；确定、稳定、无抖动。
- Reactor standby：挂网但计 generators、不计 generation；网络 generation=0 时消费者显示 `NoGenerator`。

## Integration with existing systems

- **Construction**：`BuildingKind::PowerCable / Reactor` 复用蓝图→供料→施工→完成管线；电缆完成时写入 `CableGrid`（不落地为建筑实体），Reactor 完成时落地 2×2 Building + 发电机件。**放置规则**：电缆可在任何非边框格下（含内墙/机器/门），机器蓝图不能与已有蓝图足迹重叠（电缆蓝图可以垫在机器下 → 先放机器后铺线的顺序约定）。
- **Deconstruction**：Deconstruct 工具点击电缆格 → 生成 1×1 瞬时 Building 实体（黄框）→ 船员走到格上 1s 拆除 → `CableGrid` 清位、无返还（成本为 0）。拆 Reactor 走建筑拆除（全额返还 8 零件），随实体消失自动离网。
- **Production**：`Fabricator` 增加 `PowerRole::Consumer` + `PowerStatus`。供料/入料不要求电力（搬运是人力），**Operate 候选与执行全程校验 PowerStatus**：无电不领取、走位中失电取消行走、施工中失电 `abort_cycle()`（矿不消耗、无幽灵产出）、复电后下一扫描恢复正常。产线测试覆盖"断电→停机→复电→完成恰好一次"。
- **Jobs 调度**：`actions → power_network → crew_task → crew_scan`（Set::Jobs 内链式），保证玩家改电网后同帧内设备状态先更新、任务系统后读取。时间控制（Pause/1×/2×/4×）下拓扑计算与倍速无关。
- **移动系统**（顺带修复）：新分层避让详见下节；Trace 工具（`SLICE0_TRACE`）保留了诊断输出。

## Crew avoidance rewrite（本切片的必要修复）

Slice 1 遗留：走廊对头互堵（blocked_for 衰减侵蚀）与施工占格导致的长时间僵持。本轮重构为四层：

1. **对头优先**：双方 `next` 互指对方格时，低 Entity id 立即穿行 — 一帧解决对峙；
2. **侧步让位**：被堵 ≥0.35s 且冷却 ≥1.2s，向"障碍物四邻域中同时与自身四向相邻的自由角格"让位（保证回程合法 4 向步），同一障碍只让一次；
3. **绕路**：0.6s 后把他人当墙重寻路（仅接受 +4 格内的绕行）；
4. **单调看门狗**：`stuck_for` 只因真实前进清零，2.5s 无格推进强制穿行 — 任何上层机制组合都逃不过这个兜底。

验证：旧 F 场景 trace 中 `pos=(19,15)↔(20,15) prog≈0` 的 37 帧乒乓（施工者占蓝图格 + 侧步重置计时互咬）在新 trace 中消失；M 场景改造后阶段 5 件产品从 47.5s → 29.5s（走廊通行效率 +38%）。

## Architecture

- `src/power.rs`：`CableGrid`（密集层 + version 计数）、`PowerRole`/`PowerStatus` 组件、`PowerState`（每网络 NetworkInfo + 设备→网络映射，纯派生数据）、`flood_regions` + union-find 并网、`power_network_system`（每帧全量重算）。
- 电网不做第二权威世界：网络编号/连通域永远从 CableGrid + 设备足迹现算；未来物流/通风层各自独立网格，互不共享连通性。
- Overlay 渲染按签名（grid version ⊕ 网络状态 ⊕ 设备状态）重建 root 子节点，关闭时整体隐藏。
- UI：顶栏 Power 按钮 + 网络摘要行；选择面板按 `SelSig::Generator` 复用固定按钮槽（Online/Standby/拆除）；Fabricator 面板追加 POWER 行。

## Tests

- **总数 62**（Slice 1 为 44）：
  - 单元 20（新增 cable grid 版本计数、flood 区域计数、接口含周边）；
  - `tests/haul_logic.rs` 10（回归通过）；
  - `tests/ship_ops.rs` 17（回归通过；测试装置加电后全部保持绿色）；
  - `tests/power_ops.rs` 15（新增）：发电-供电端到端、剪线断电/重接恢复、**电网分割**（源侧运行/远侧 NoGenerator）、**桥接并网**、**设备跨两网电气并网**、孤立网、多独立网、gen/demand/headroom 数学、**确定性过载卸载**（6×20>100，最老设备保电、最新被卸、10 帧无抖动）、**Reactor standby**、断电不派操作工、**中途断电不耗料**、电缆蓝图建入网格、电缆拆除即失电、船壳边框拒绝铺设。
- 门禁：`cargo fmt --check` ✓ `cargo clippy --all-targets --all-features -- -D warnings` ✓ `cargo test` 62/62 ✓。

## Automated acceptance

`SLICE2_SCENARIO=A..J`（每项输出 S2_RESULT 摘要后自动退出）：

- **A Healthy grid** ✓ reactor 在线、fab Powered、NET gen100/dem20/served20；
- **B Disconnected consumer** ✓ 剪断 (15,14) → fab `Unconnected`，其余网络完好；
- **C Grid split** ✓ 西北新建 fab 接入反应堆侧；剪断 (15,15) → 双网络：源侧 100/20 运行、远侧 0/20 NoGenerator；
- **D Reconnect** ✓ 剪断→蓝图重铺→同网络恢复 Powered；
- **E Generator offline** ✓ Standby → fab NoGenerator（generation=0）；Online → 恢复；
- **F Overload** ✓ t=146s：6 台制造机 + 20 段电缆建成后**单一网络** generation=100 / demand=120 / **served=100** — 20 PU 缺口确定性卸载（最新设备 Shed），无凭空超额供电、无抖动；
- **G Runtime construction** ✓ 孤立 fab 先建（Powered=无源接口→Unconnected→按并网规则报告），铺线后并入主网 Powered；
- **H Runtime demolition** ✓ 拆中段线 → 立即失电、无幽灵连接；
- **I Time controls** ✓ Pause/1×/2×/4× 循环后拓扑与供电一致；
- **J Regression** ✓ SLICE0 A–L 全部重跑通过（含 M 布局优化 +47% 结论不受影响）。

## Playtest pass 1 — 理解性

以新玩家视角检查可发现性：顶栏 `Power [P]` 按钮与 BUILD 栏 `Power Cable` 工具均在默认视图可见；选中 Reactor 面板第一行即"Starter Reactor (12,16)"。截图验证（`SLICE0_SHOT`）确认 overlay 网络色/设备环渲染。**发现并修复**：选中环贴图实心遮挡（见 Usability fixes）。

## Playtest pass 2 — 修改电网

脚本化玩家流（PW 场景）：开电力视图 → 拆中段线 → 观察断电 → 重铺 → 恢复。反馈链路清晰：电缆黄框（待拆）→ fab 头顶 `NO POWER — no generator` → 红环 → 重铺后绿环。**发现并修复**：拖画布线最初每帧重复放置被 CableExists 拒绝刷屏 —— 改为按格去重（`last_paint`）。

## Playtest pass 3 — 运营压力

F 场景即压力测试：同时建 6 台制造机 + 20 段电缆 + 大量搬运。这一轮暴露并修复了**四个真实缺陷**（全部由 `SLICE0_TRACE`/`SCAN_DEBUG` 日志 + 无头探针世界定位，部分经 codex 复审确认方案）：
1. **侧步乒乓死锁**（见 Crew avoidance rewrite）；
2. **CarriedBy 幽灵泄漏**：蓝图供料物品在拾取瞬间若蓝图已满料会转成入库搬运，而入库校验要求玩家标记 → 未标记物品被裸取消（不走 end_haul、不放下）→ 物品永久卡在"被携带"状态。修复：携带中物品豁免标记校验；
3. **"仓储泵"死循环**：领取时不校验目的地可达性 → 被墙封死的蓝图反复吸引"取料→转存→再取料"。修复：领取时用 `path_to_interaction` 验证 源→目的地，不可达即不领取；
4. **union-find 并网缺陷**：设备接口的 touched 区域列表含非连续重复项 `[0,1,0]`，我的 union 循环只并首尾（0↔0）从不并 0↔1 → 该并的网没并。修复：排序去重后逐一与首项并。这个 bug 正是 codex 复审 P1 提示"侧步产生的非相邻路径与 yield_for 立即清除"时让我重新审视数据流的产物。
另外发现一个**玩法级教训**：在 (17,10) 建造 2×2 制造机会封死 FABRICATION 房间唯一的门 — 系统行为正确（封死的蓝图不再吸引供料），但缺乏"此处建造将封死房间"的玩家提示（记入 Known issues）。

## Design assumptions made

- **过载分配**：Entity id 升序（≈先建先得）确定性服务；未做优先级 UI（验证最小模型已足够理解）。
- **功率数值**：Reactor 100 PU / Fabricator 20 PU（一台反应堆带 5 台满载）。
- **电缆成本 0 材料、1.5s 施工**：鼓励布线实验；未来可改耗材。
- **设备接口 = 足迹 ∪ 四邻域**：不必把线精确压到机器格下，降低布线挫败感；代价是相邻机器可能"共享"一根贴边线（电气上合理）。
- **设备触及多网即并网**：机器内部母线把两段电缆短接（RimWorld 式语义）。
- **Standby 语义**：挂网不计发电；其网络消费者显示 NoGenerator。
- 电缆可穿**内墙**（船壳=地图边框才禁）；Door 电动化暂不做（常开）。
- 搬运/建造/入料不需要电（人力）；只有 Operate（机器运行）耗电。

## Temporary behaviors

- Reactor 无燃料/热/维护（明确 Deferred）。
- 电缆拆除瞬时实体（1×1 Building）只为复用拆除工作流，未来可改为专用"拉线"作业。
- 过载按 Entity 序而非玩家可配优先级。
- 无电池/蓄能概念。

## Known issues

- 门口 1 格瓶颈仍会排队（现在有界：≤1.5s + 穿行），多机并建时可见。
- Overlay 网络颜色按索引取色，网络数多时可能撞色。
- 电缆拖画在 UI 面板上悬停时不画（正确），但没有"画完显示总价/总长"提示。
- 6 台制造机同网时第 6 台永远 Shed（先建先得），玩家需自行分网 — 可读但需教学提示（未来 onboarding）。
- **没有"此建造将封死房间/堵门"预警**：把 2×2 机器建在门口会静默封死整个房间（行为正确但玩家容易意外）。
- Trace/截图工具的教训已沉淀到 `AGENTS.md`（先诊断后猜测、外部求助策略）。

## Deferred systems（明确未实现）

heat / cooling / 散热 / 维修 / 详细燃料 / 电池 / 环境（氧气气压真空火灾）/ 战斗电力管理 / 航行耗电 / 反应堆事故 — 全部留给后续切片；当前电力模型未给它们设置任何障碍（设备组件化、网络纯派生、无全局单例）。

## Art assets added or changed

- 新增 `reactor.png`（Codex 生图 → prep_art）。
- **`ring.png` / `dot.png` 改为程序化合成**（白环/白点，游戏内 tint 上色）— 原 AI raw 是纯洋红占位方块，经管线后成实心板遮挡内容；UI 图元不再走生图管线（已写入 AGENTS.md 经验）。
- `floor/wall/wall_built/door/rack/fabricator/crate/ore/part/crew` 重跑 prep_art（视觉等价重编码）。

## Post-review UI refinement

交付后应用户要求把 BUILD 栏改为**分类 + 浮窗**：栏内只保留
Structure / Storage / Machines / Power 四个分类按钮（外加 Deconstruct、Cancel Tool），
点击分类展开浮窗选择具体建筑；选中建筑、切换工具（B/Esc）或再点分类即收起；
活动工具所属分类保持高亮。纯 UI 层改动（`BuildMenu` 资源 + `build_menu_system`），
不触碰模拟；全部 64 测试与门禁保持绿色。

## Git

- branch：`main`
- commit：见 `git log -1`（"Playable Slice 2 — Ship Power: ..."）
- push：`origin/main`（github.com/ceyirelehe47/ship-alive）
