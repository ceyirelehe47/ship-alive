# 性能优化第二轮 — Performance Pass 2

日期：2026-08-17 · 基线：Slice 8 完成态（`1c373e9`）· 无玩法改动。

## 1. 目标与方法

上一轮（`REPORT_PERF.md`，Slice 3 后）覆盖了热/冷却/覆盖层/UI。本轮聚焦
**Slice 4–8 之后的新热路径与遗留分配热点**：寻路、任务扫描、任务执行内环、
Slice 8 的每帧字体/静态标签遍历。

方法沿用上轮：先建可复现基准（`tests/perf_jobs.rs`，3 个 release 基准），
再做行为等价优化，最后全套测试 + 验收场景**逐位回归**。

**基准教训（本机）**：短基准跨运行对比会被 CPU 频率爬升严重失真（同一份
代码 300 次寻路测出 10k–56k paths/s）。A\* 对比改为**进程内交错 A/B**（把旧
HashMap 实现复制进基准，5 个批次交替跑两种实现）——这是报告里的可信数字。

## 2. 基准与结果（release）

| 基准 | 内容 | 优化前 | 优化后 | 提升 |
|---|---|---|---|---|
| `perf_astar_long_paths` | 128×128 开阔图对角长路径，dense vs 旧 HashMap 进程内交错 A/B（各 10000 次） | 20269 paths/s | 39475 paths/s | **1.95×** |
| `perf_scan_under_entity_load` | 8 船员每步重扫 + 240 标记物品 + 24 货架 + 12 自动补料需求（1000 次扫描） | ~18k scans/s | ~35k scans/s | **~1.9×** |
| `perf_haul_churn_end_to_end` | 8 船员 128×128 全链搬运 20000 步（领取+寻路+行走+入库） | ~23–31k steps/s | ~32–33k steps/s | ~1.1×（本就远离瓶颈） |

## 3. 找到的热点与修复

| # | 位置 | 优化前 | 优化后 |
|---|------|--------|--------|
| 1 | `path::find_path`（A\* 内环） | `best_cost: HashMap<TilePos,u32>` + `came_from: HashMap<TilePos,TilePos>`——每次松弛 2 次哈希（每个任务领取、路径恢复、可达性检查都走这里） | 稠密 `Vec<u32>`（g-score + 打包父索引，`u32::MAX` 哨兵）按下标读写；堆顺序、DIRS 顺序、松弛判据逐行保留 → **路径输出逐位不变**（回归 `hauls_dist` 完全一致证实） |
| 2 | `jobs::at_interaction`（每个活跃任务每步调用） | `interaction_tiles(map, foot)` 每次分配 Vec + O(n²) 去重再 `contains` | 新增免分配 `building::is_interaction_tile`（脚印矩形内/四邻判定），语义等价（脚印=满矩形） |
| 3 | `crew_task_system`（每个固定步） | 每步重新收集 `crew_positions`、`ground_now` 两个 Vec（4× 速度=240 步/s 的分配churn） | `Local<Vec>` 清空复用，零稳态分配 |
| 4 | `crew_scan_system`（每空闲船员） | 每船员全实体扫描：标记物品循环里**每个物品两次** `racks.iter().any(can_take)`（O(物品×货架)）；`best_source_for` 每需求全物品+全货架扫描；蓝图/制造机需求每船员重枚举 | 帧级共享索引：`rack_accepts[3]`、`marked_free`、`GroundIndex.by_kind[3]`（按种类分桶的地面物品）、`bp_needs`/`fab_needs`（需求表，`input_want` 线性于 inbound 故每船员只做 `base−already`）；**惰性构建**（本步无人扫描则只付一次 `any()` 预检——第一版无条件构建曾让 churn 掉 40%，已修正）；领取时从共享表移除（等价于旧的 `local_claims` 复查） |
| 5 | `settings::font_apply_system`（Slice 8 引入） | 每帧遍历**全部** `TextFont` 比较字体句柄 | `Changed<TextFont>` 驱动（新文本 spawn + 自身写入各触发一趟，随后自稳） |
| 6 | `settings::static_label_system`（Slice 8 引入） | 每帧遍历全部 StaticLabel 调闭包取串比对 | 首帧 + `lang.is_changed()` 才跑（静态标签不在启动后生成） |

未做（评估后放弃）：`choose_rack` 的克隆+排序（货架 ≤30 个，收益微小）；
`movement_system` 的占用快照 Vec（船员 ≤8，O(n²) 不成立）。

## 4. 行为等价验证

- **测试 202 → 205 全绿**（新增 `tests/perf_jobs.rs` 3 个基准兼金丝雀）。
- **场景回归逐位一致**（路径敏感指标 `hauls_dist` 尤其关键）：
  - S0-A：`stored=39 hauls=23 haul_dist=465`，四人 h=7/6/5/5 ✓
  - S0-K：`haul_dist=119 built=1 produced=2`，Ava h=4 / Rex b=1 / Zed o=2 ✓
  - S2-A：100/30/30 ✓ · S4-A：7 舱室 / haul_dist=238 ✓
  - S5-A：species 48300.0 守恒 ✓ · S6-A：ship_total **48700.0 精确** ✓
  - S7-A：`hauls=4 haul_dist=48 stored=19 reserved=2` ✓ · S7-B/E ✓
  - S3-A：core 33.4°C / water 115.0 / 冷却诸量到小数点后 5 位一致 ✓
    （注入总量 1058400 与旧记录 1058596 的差异是 **dump 边界噪声**：旧运行
    在 FixedUpdate 追帧时多跑了一步才被驱动器看到，打印 t=5401s vs t=5400s；
    优化后连续两次运行逐位一致，物理量全部吻合。）
- A\* 的等价性由构造保证：堆的 `Ord`、DIRS 顺序、`next_cost < best` 判据、
  回溯逻辑逐行保留，仅把哈希查找换成下标访问。

## 5. 正确性说明

- **共享扫描索引的失效语义**：索引只在"本步确有船员要扫描"时构建一次；
  帧内世界状态只经延迟 Commands 变化（ReservedBy/交付/缓冲），故对后续
  船员恒有效；立即生效的变化只有 RackPull 的 `cell.take()`（货架不走缓存，
  每次现查）与领取本身（领取时从 `marked_free`/`GroundIndex` 移除，等价于
  旧的 `local_claims` 复查）。
- **`fab_needs` 缓存 `input_want(0)`**：`input_want(inbound) =
  need_total − input − inbound` 线性，故每船员 `base.saturating_sub(already)`
  与现算完全一致（`input`/订单本帧不变——变化走延迟命令）。
- **`font_apply` 的 Changed 自稳**：spawn 触发首趟写入；写入标记 Changed
  触发次趟，比较相等不再写，自此静止。语言切换不改字体，无需重跑。
- **A\* 每次调用分配 2×(w·h)×4B**（128×128=128KB）：领取频率是人类尺度的，
  远低于每步；相比旧版每次搜索的数千次哈希插入仍然净赢（A/B 证实）。

## 6. 遥测与复现

```bash
cargo test --release --test perf_jobs -- --nocapture
# PERF A* dense: ... | legacy HashMap: ... | speedup 1.95x
# PERF SCAN: ... (240 items/24 racks/12 demands)
# PERF CHURN: ... hauled 64, stored 64
```

## 7. Git

- 代码与文档提交：**`927af31`**（main，直接提交，未 force-push）。
- 门禁：`cargo fmt` ✅、`cargo clippy --all-targets --all-features -- -D warnings` ✅、
  `cargo test` **205/205** ✅。
