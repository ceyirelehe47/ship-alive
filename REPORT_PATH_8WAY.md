# REPORT_PATH_8WAY — 8-direction A* Pathfinding Upgrade

日期：2026-08-16
基线：`main @ 52ebcbe`（Slice 2 + Build 分类浮窗）

聚焦的移动基础设施升级：4-way A* → 正确的 8-way A*。无新 Gameplay。

## Implementation

- `src/path.rs` 完整重写为 8 向：
  - 邻居扩展为 8 方向（先 4 正交后 4 对角，确定性遍历顺序，等代价路径偏好直线而非锯齿）；
  - `step_enterable` 统一入口判定：目标格可行 + 对角时**两个侧格都必须可行且未被 block**；
  - 新增纯函数助手：`step_length`（世界步长 1 / √2）、`step_cost`（定点 10/14）、
    `path_length`（世界几何长度）、`path_cost`（定点总代价）、`octile_cost` / `octile_distance`。
- `src/movement.rs`：
  - `Movement.progress` 语义从"归一化 0..1"改为**距离预算**（tile 单位）：
    每帧 `progress += dt · speed`，进入下一格需支付 `step_length(pos, path[0])`
    （对角 √2、正交 1）。剩余预算跨步结转并按步长换算 —— 混合步序列
    （cardinal→diagonal→cardinal）在一个 tick 内跨多格也不漂移；
  - 软避让的绕路接受判据从**节点数** `alt.len() < path.len()+4` 改为**真实代价**
    `path_cost(alt) < path_cost(cur) + 4·CARDINAL`（4 个正交步当量）。
- `src/render.rs`：`crew_world_pos` 插值因子改为 `progress / step_length(...)` 归一化，
  对角插值平滑、无瞬移/抖动；路径点、携带物、名字标签全部沿用同一插值函数。

## Cost model

定点整数（×10 tile-distance）：**cardinal = 10，diagonal = 14**。
14 < 10·√2（≈14.142），保证启发式可采纳（admissible）；确定性、无浮点比较问题。

## Heuristic

**Octile distance**（同尺度定点）：`h = 10·max(dx,dy) + 4·min(dx,dy)`。
对 10/14 cost 模型可采纳且一致（consistent）——A* 仍给出最优路径（测试 C 用
同规则暴力 Dijkstra 在 3 张障碍图的全目标对上逐点验证 cost 相等）。

## Corner rule

**严格禁止 corner cutting**：对角步 `(x,y)→(x+1,y+1)` 要求
`(x+1,y)` 与 `(x,y+1)` **两个侧格都可通行且未被动态 blocked**。
即：双墙角（`S#/#G`）与**单侧墙角**（`S#/.G`）都不可斜穿；只有全开的角
（`S./.G`）可以。动态避让的 `blocked` 闭包同样作用于侧格 —— 重规划不会
规划出穿人/擦角路径。

## Movement timing

距离预算制保证**世界空间速度各方向一致**（测试 H）：
- 10 正交步（世界距离 10）与 10 对角步（世界距离 10√2）同速船员
  的实测世界速度均为 3.0 tiles/s（±0.10），无 41% 对角加速；
- 对角腿耗时 ≈ 正交腿 × √2（±5%）。
- 混合步序列在 1×/2×/4× tick（1/60、2/60、4/60）下到达时间都与几何长度
  一致（测试 I），Pause 不推进（虚拟时钟语义未动）。

## Soft avoidance

分层避让全部保留并适配 8 向（测试 K/K2 + 场景回归）：
- 对头互堵：低 Entity id 立即穿行（对头判定按"互换目标格"天然覆盖斜向对头）；
- 侧步让位：候选格改为"与自身和障碍格都 4 向相邻的角格"，保证让位步与
  回程步在 8 向下都合法；
- 绕路重寻：用 8 向 A*（侧格规则同样作用于动态 blockers），接受判据按真实代价；
- 穿行兜底与单调看门狗（2.5s 无真实推进强制穿行）不变。

## Job distance

所有 Manhattan 估价替换为 **octile**（不做逐候选 A*，扫描成本不变）：
- `Footprint::distance_to`（Build/Operate/MachineOut 候选距离）→ 逐足迹格 octile 最小值；
- `choose_rack` 入库目标排序 → octile；
- `best_source_for`（蓝图/机器供料源选择，含货架 +1 偏好）→ octile；
- 扫描中标记物品候选排序 → octile；`Candidate.dist`/score 相应 f32 化。
工作交互距离语义（足迹∪四邻域）**未改**——本轮只改"怎么走过去"。

## Telemetry

`Stats.haul_distance` 从"路径节点数"改为 `path_length(pos, path)` 的
**真实几何距离**（Σ step_length，对角计 √2）。领取时从船员当前位置起算。
场景 A 全员搬运总距离 493（节点计数）→ 463（几何）且更短（对角抄近路的效果
被正确体现）。Scenario M 的布局 A/B 对比数字因此有可比性变化（见 Playtest）。

## Tests

- **总数 76**（原 64，+12）：
  - `src/path.rs` 单元 +7：开放区走对角、对角 cost=√2 当量、
    **对 3 张障碍图全目标 Dijkstra 基准逐点一致**、双墙角禁穿、单墙角禁穿、
    开角允许、动态 blocker 护侧格（+ 保留的旧 4 向用例继续通过）；
  - `tests/path8.rs` +5：对角无速度增益（双腿独立世界计时）、混合步 1×/2×/4×
    稳定、动态墙禁斜穿且拆除恢复、8 向对头与对角交汇不死锁、1 格门三船员排空。
  - 修正了新测试自身的两处计数笔误（9 步当 10 步、2 对角步当 3 步）——
    均为测试预期错误，实现无误（独立探针验证后修正）。
- 全量回归：`haul_logic`(10) / `ship_ops`(17) / `power_ops`(15) / `fleet_ops`(2)
  全绿；`fmt --check`、`clippy -D warnings` 通过。
- 场景回归：SLICE0 A/D/F/I/L、SLICE2 F（过载 120>100 served=100）全部通过。

## Playtest

### Playtest 1 — Navigation feel（SLICE0 A + SLICE0 M，TRACE 实测）

开局搬运 TRACE 采样：船员在船员舱→货舱走廊、走廊→矿仓等多个位置自然走斜线
（捕获 `(16,6)→(15,7)`、`(7,7)→(6,6)`、`(7,4)→(8,3)` 等对角步），路径更直接；
未观察到穿角/穿墙（禁角规则另由动态建墙测试 J2 覆盖）；对角速度与正交一致
（速度一致性由测试 H 定量保证）；引擎内截图正常渲染。
场景 A 全部 23 件入库完成（总搬运距离 493 节点 → 463 几何，且实际更快），
无人卡死。场景 M（布局 A/B）：基线 5 件 70.6s → **62.6s**（对角抄近路带来
约 11% 的自然提速），改造后阶段同样完成，布局优化结论依然成立。

### Playtest 2 — Congestion（SLICE2 F + SLICE0 L）

6 台 Fabricator + 20 段电缆同时建造的拥堵压力（4 船员、单格门、对角汇入）：
场景在 t=114.8s 完成全部建造并呈现 120>100 的确定性过载（4-way 时 146.4s，
-22%）；SLICE0 L（4× 压力）built=3 produced=3 全员工作，无重复任务/卡死、
无对头死锁复发。
发现并修正：无（本轮无新死锁；侧步让位在对角障碍下由"角格"规则天然约束）。
注：TRACE 输出目前挂在 SLICE0_SCENARIO 驱动器上，SLICE2 场景不打印 TRACE
（记录为 Known issue 的工具缺口，不影响结论——拥堵由场景完成时间与 L/F
统计证明）。

## Known issues

- `SLICE0_TRACE` 追踪打印只挂在 SLICE0 场景驱动器上，SLICE2 场景无 TRACE
  输出（诊断工具缺口，后续可把 trace 挪到独立系统）；
- 对角交叉（两船员在两条对角边中央互穿）靠现有占格快照 + 穿行兜底处理，
  极端同帧对穿会有短暂视觉重叠（与 Slice 1 行为一致，非新增）；
- 绕路接受阈值"+4 正交步当量"为经验值，极拥堵时可能偶尔回退到等待；
- `path dots` 预览在对角步上呈折线（正确但视觉上略密）。

## Git

- branch：`main`，commit：见 `git log -1`（"Pathfinding: 4-way → correct 8-way ..."）
- push：`origin/main`
