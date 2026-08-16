# 性能优化轮 — Performance Pass

日期：2026-08-16 · 基线提交：`7441f17`（Slice 3 完成态）· 无玩法改动。

## 1. 目标与方法

Slice 3 交付后做一轮系统级性能清理。方法：

1. **读码定位热点**：逐个审查每步（FixedUpdate）和每帧（Update）运行的热路径。
2. **可复现基线**：`perf_128x128_synth_loop`（128×128 全醒着合成环路，release，1000 步计时）
   —— 优化前 **1847 steps/s（0.54s/千步）**。
3. **行为等价验证**：全套测试 + SLICE0/2/3 验收场景输出必须逐位一致；
   overlay 用引擎内截图 + 像素级着色覆盖图确认视觉不回归。

## 2. 找到的热点与修复

| # | 位置 | 优化前 | 优化后 |
|---|------|--------|--------|
| 1 | `thermal_air_system` 传导内环 | 每个醒着 tile 的每个邻居做 2 次 `HashMap<usize,f32>` 设备质量查找（12k 醒×4 邻×60 步/s） | `DeviceTiles` 改稠密 `Vec<f32>`，纯下标读 |
| 2 | `coolant_system` 拓扑 | 每步全量 flood-fill + 每 tile 分配 Vec + 排序 + 设备重挂接 + 每网络两份旋转缓冲 | 拓扑缓存：`PipeGrid::version × 设备实体位签名` 不变则复用（走序/挂接/水库表），旋转改纯取模索引零分配；删除从未被读过的 `bonus_cap` 死代码 |
| 3 | `thermal_overlay_system` | 打开时**每帧**全网格签名扫描；任一 tile 跨 1°C → 整棵 despawn+respawn ~2500 个 sprite | 池化：每个开放 tile 一个常驻 sprite，成员变更（`ShipMap::version`×设备集）才重建池；颜色按 1°C 桶差异就地更新，10 Hz 节流，只写变化的 sprite |
| 4 | `coolant_overlay_system` | 同上（全网格每帧签名 + 全量重建） | 同上池化（温度 1°C + 水量 ¼ 单位合桶），10 Hz 刷新 |
| 5 | `sync_crew/item/rack/building_visuals` | 每帧 O(目标实体 × 全部可视实体) 嵌套扫描（物品堆积时二次方增长）；颜色/文本无条件重写触发变更检测 | 新增 `VisualIndex`（(target, role)→visual 的 HashMap，`Added<Visual>` 增量维护，清理时移除）→ O(1) 查找；颜色/可见性/文本只在值变化时写 |
| 6 | `hud_update` / `sidebar` / `overlay_summary` | 每帧重建 ~30 段 UI 文本 → Bevy 文本重排布；sidebar 每帧全网格扫最热房间 | 0.2s 墙钟节流 + 值不变不写（`set_text_if_changed`）；覆盖层切换瞬间强制刷新保证 `P` 键手感 |

支撑改动：`ShipMap` 新增 `version`（`set_tile` 递增）作为覆盖层成员变更信号；
`DeviceTiles::sized(n)` 构造（setup 与测试 harness 显式按地图大小分配）。

## 3. 结果

### 模拟吞吐（release，128×128 最坏情况）

| | 优化前 | 优化后 | 提升 |
|---|---|---|---|
| 千步耗时 | 0.54 s | 0.29–0.30 s | **1.81–1.86×** |
| steps/s | 1847 | 3350–3444 | |
| 醒着的 tile | 12137 | 12137（物理逐位一致） | |

4× 速度档需要 240 步/s：优化前该合成场景占约 13% 预算，现在约 7%。
启动船（60×60、~250 开放格）远低于此。

### 每帧开销（渲染/UI，数值为结构性消除）

- 热视图打开时：每帧 3600 次签名迭代 + 漂移期每秒数十次整树重建（~2500 实体 spawn/despawn）
  → 每帧 O(设备数) 签名 + 每 100ms 一次桶差异颜色写（数量见 `ThermalOverlayVis.color_writes` 遥测）。
- 正常视图：crew/item/building 同步从嵌套扫描改为索引查找，颜色/文本只在变化时写
  （此前每帧无条件写触发 Bevy 变更检测与 sprite 重提取）。
- UI 文本重排布从 60/s 降到 5/s（值未变时为 0）。

### 行为等价

- 测试 112 → **113 全绿**（新增 `coolant_topology_is_cached_between_edits`：
  500 步环流零重建；切管/接回各恰好 1 次重建；水量守恒）。
- 场景回归逐字一致：SLICE0 M（built=2 demo=2 produced=10 hauls=16 dist=546）、
  SLICE0 F（40/40 满）、SLICE2 A（100/26/26）、SLICE2 F（100/106/86）、
  SLICE3 R（600 sim-s@4×：核心 30.7°C Normal、水 115.0 守恒、零泼溅、环路 3.4 流量）。
- overlay 视觉：V 场景循环 P→T→C→Off 无 panic；截图像素扫描确认内部
  青/绿热图着色大面积覆盖、暖区偏绿、环/HUD/侧栏正常。

## 4. 正确性说明

- **拓扑缓存失效**：签名 = `pipes.version`（任何铺/拆管递增）× 泵/换热器/散热器/水库
  实体位异或。水的流动、温度、泵断电都**不**改变拓扑，正确地不触发重建；
  设备增删/管道编辑必然改变签名。水库容量表（拆管保水用）随拓扑一起重建，随设备集保持新鲜。
- **覆盖层 10 Hz 刷新**：纯视觉节流；成员变更（墙/管/设备）仍当帧重建池。
  开池后首帧颜色在 spawn 时直接写入（非空窗）。
- **写前比较**：颜色/文本/可见性先比较后写，避免触发 Bevy 变更检测；语义与
  无条件写完全一致（无读取方依赖"每帧都写"这一行为）。
- **UI 0.2s 节流**：仅内容行；按钮高亮、模式切换可见性仍每帧；overlay 模式
  切换后强制立即刷新一次。

## 5. 遥测

- `CoolantState.topology_rebuilds`：拓扑重建累计数（测试断言用）。
- `ThermalOverlayVis` / `CoolantOverlayVis` 的 `rebuilds` / `color_writes`：池重建与
  颜色写累计数（调试期评估用，稳定后可移除）。
- HUD 调试行 `SIM steps/frame / peak / backlog` 不变，仍是卡顿观察入口。

## 6. 后续候选（本轮未做）

- Power 覆盖层池化（当前重建量小：电缆数级，收益有限）。
- `sync_selection_system` / `ghost_system` 同样接 `VisualIndex`（当前查询已较小）。
- crew 扫描/寻路的节流缓存（4 人规模下未构成热点）。
- 大规模物品场景的 Text2d 标签合批。
