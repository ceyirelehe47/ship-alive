# Ship Alive — Playable Slice 2 / Ship Power

> **Slice 2 新增：舰内电力成为真实空间系统。**

一个用 **Rust + Bevy 0.16** 实现的飞船殖民模拟切片。4 名船员在一艘固定
Starter Ship 里生活和工作；Slice 1 让玩家第一次真正**经营和改造自己的飞船**：

**调整舰内布局 → 建造/拆除设施 → 配置仓储过滤 → 安排工作优先级 →
生产 Machinery Part → 观察物流瓶颈 → 改造布局 → 明显改善运营效率。**

- 舰内建造：Wall / Door / Storage Rack / Fabricator（2×2 多格机械）
- 真实施工：蓝图 → 自动物流送料（地面/货架库存）→ 船员施工 → 完成即影响寻路
- 拆除返还全部建材（含货架内存货），鼓励反复试验布局
- 最小生产链：**2 Asteroid Ore → Fabricator → 1 Machinery Part**，
  缺料自动供料、需要船员实地操作、产出自动入库；Output blocked 可视化
- 仓储过滤：每个货架可勾选接收的物品种类（原料架贴机器、成品架守出口）
- 三类工作（Haul / Build / Operate）× 每名船员 Off/Low/Normal/High 优先级
- 0B 全部交互保留：框选搬运、悬停信息卡、相机、时间控制、Debug 工具折叠
- **Slice 2 — 电力**：Starter Reactor（2×2 实体设备，100 PU）、地下电缆层
  （不占地面、可穿内墙/机器/门下）、真实电网拓扑（切断即局部失电、重接即恢复、
  一船多网、设备接口跨网自动并网）、过载确定性卸载（先建先得）、
  Power Overlay（`P` 键：按网络着色的电缆 + 设备供电状态环 + 网络摘要行）、
  Fabricator 断电停机/复电安全恢复、断电反馈（NO POWER 文字 + 暗环）

```bash
cargo run                      # 启动游戏（玩法见 PLAYTEST.md）
cargo test                     # 91 个单元/集成测试
SLICE0_SCENARIO=A cargo run    # Slice 0/1 验收场景（A–L、P1/P2/M）
SLICE2_SCENARIO=A cargo run    # Slice 2 电力验收场景（A–J、PW）
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings
```

## 目录结构

```
src/
  lib.rs         模块组织 + 帧内系统顺序（Input→Jobs→Move→Sync）
  map.rs         固定舰船布局（字符画）→ 稠密网格 ShipMap（含 BuiltWall/Door/Machine 格）
  path.rs        网格 A*（8 向，octile 启发式 + 严格禁穿角，10/14 cost）
  crew.rs        船员组件：Crew / 工作优先级 / CrewTask(Idle|Haul|Build|Deconstruct|Operate)
  items.rs       地面物品：Item / MarkedForHaul / ReservedBy / CarriedBy
  storage.rs     货架 StorageCell（容量 + 物品类型过滤）
  building.rs    建筑定义/蓝图/放置校验/施工完成/拆除返还（多格 Footprint）
  power.rs       地下电网：CableGrid/PowerRole/PowerStatus/网络 flood-fill+并网+负载计算
  production.rs  Fabricator：配方(2 Ore→1 Part)/订单(Produce N|Repeat)/状态机/缓冲
  jobs.rs        核心：玩家动作、四类任务执行、统一工作扫描与优先级领取
  movement.rs    逐格移动 + 软避让（含对头死锁的按格累计穿越机制）
  simtime.rs     统一模拟时钟（i64 µs、T+HHH:MM:SS、固定步长泵/累加器/退避）
  time_ctrl.rs   玩家倍率 Pause/1×/2×/4×（Space 记忆上次倍率）
  input.rs       选择/框选/建造工具/ghost/拆除点击/快捷键/相机
  render.rs      建筑/蓝图/机器状态可视化 + 放置 ghost + 房间标注
  ui.rs          HUD：右侧边栏(环境信息/选中实体属性+操作)、BUILD 分类栏/船员状态/事件日志
  ui_overlay.rs  悬停 tooltip + 框选矩形
  autotest.rs    SLICE0_SCENARIO=A..L + P1/P2/M 自动验收与试玩驱动器（开发工具）
  stats.rs       开发遥测（产量/搬运距离等，用于布局 A/B 对比）
  setup.rs       从字符画生成世界（含预置 Fabricator 与 P/O 库存货架）
  bin/prep_art.rs 生成美术的后处理（去背/裁切/缩放）
tests/
  path8.rs       8 向移动测试（对角速度一致/混合步/禁穿角/避让）
  haul_logic.rs  Slice 0B 无头集成测试（领取互斥/框选/满仓/…）
  ship_ops.rs    Slice 1 无头集成测试（建造/拆除/生产/过滤/优先级）
  power_ops.rs   Slice 2 电力测试（拓扑/分割/并网/过载/断电生产）
  fleet_ops.rs   多制造机建造+材料守恒+封死房间不泵料
tools/           截图脚本（开发用）
assets/art/      运行时加载的 PNG（缺失时自动退化为色块占位）
art_raw/         Codex image generation 原图（洋红底）
```

## 美术管线

`art_raw/`（Codex image generation 生成，洋红底）→ `cargo run --bin prep_art`
→ `assets/art/`（透明背景 256×256）。游戏启动时若文件存在则加载，
否则用程序化色块，保证仓库在任何状态下都能跑。

状态：**Slice 2（Ship Power）完成，等待试玩反馈。**
交付报告：`REPORT.md` / `REPORT_0B.md` / `REPORT_1.md` / `REPORT_2.md`（本轮）；
试玩指南：`PLAYTEST.md`；代理经验：`AGENTS.md`。
