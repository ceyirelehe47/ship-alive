# Ship Alive — Playable Slice 0 / 0B

一个用 **Rust + Bevy 0.16** 实现的飞船殖民模拟切片：4 名船员在固定布局的
Starter Ship 中自动领取搬运工作，走到物品、拾取、搬运并入库。
0B 轮补齐了正式玩家交互：框选批量搬运、悬停信息卡、房间标注与仓储高亮、
任务五态可视化（标记/领取者颜色/携带/入库）、空闲船员变暗、
右键拖拽平移、调试工具折叠。

```bash
cargo run                # 启动游戏（玩法见 PLAYTEST.md）
cargo test               # 19 个单元/集成测试
SLICE0_SCENARIO=A cargo run   # 自动跑验收场景 A（B–F 同理）
```

## 目录结构

```
src/
  lib.rs         模块组织 + 帧内系统顺序（Input→Jobs→Move→Sync）
  map.rs         固定舰船布局（字符画）→ 稠密网格 ShipMap
  path.rs        网格 A*（4 向，支持动态阻挡）
  crew.rs        船员组件：Crew / CrewTask(Idle|Haul) / Movement
  items.rs       地面物品组件：Item / MarkedForHaul / ReservedBy / CarriedBy
  storage.rs     货架格 StorageCell（容量逻辑）
  jobs.rs        核心：玩家动作、任务推进、领取/预约
  movement.rs    逐格移动 + 软避让（等待→绕路→穿行）
  time_ctrl.rs   暂停/1×/2×/4×（Bevy 虚拟时钟）
  input.rs       点击选择/拖框标记/右键平移/悬停检测/快捷键
  render.rs      逻辑实体 ↔ 独立视觉实体（房间标注/状态环/悬停环）
  ui.rs          HUD：顶栏/船员状态/选中面板/事件日志/Debug 折叠
  ui_overlay.rs  悬停 tooltip + 框选矩形覆盖层
  autotest.rs    SLICE0_SCENARIO=A..F 自动验收驱动器（开发工具）
  bin/prep_art.rs 生成美术的后处理（去背/裁切/缩放）
tests/haul_logic.rs  无窗口的 bevy_ecs 集成测试（领取互斥/框选/满仓/…）
tools/            截图脚本（开发用）
assets/art/      运行时加载的 PNG（缺失时自动退化为色块占位）
```

## 美术管线

`art_raw/`（Codex image generation 生成，洋红底）→ `cargo run --bin prep_art`
→ `assets/art/`（透明背景 256×256）。游戏启动时若文件存在则加载，
否则用程序化色块，保证仓库在任何状态下都能跑。

状态：**Slice 0B（Actually Playable）完成，等待试玩反馈。**
交付报告：`REPORT.md`（Slice 0）、`REPORT_0B.md`（本轮）；试玩指南：`PLAYTEST.md`。
