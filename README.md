# Ship Alive — Playable Slice 0

一个用 **Rust + Bevy 0.16** 实现的飞船殖民模拟切片：4 名船员在固定布局的
Starter Ship 中自动领取搬运工作，走到物品、拾取、搬运并入库。

```bash
cargo run                # 启动游戏（玩法见 PLAYTEST.md）
cargo test               # 17 个单元/集成测试
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
  input.rs       鼠标拾取/选中、镜头、快捷键
  render.rs      逻辑实体 ↔ 独立视觉实体（自动清理孤儿）
  ui.rs          HUD：顶栏/船员状态/选中面板/事件日志
  autotest.rs    SLICE0_SCENARIO=A..F 自动验收驱动器（开发工具）
  bin/prep_art.rs 生成美术的后处理（去背/裁切/缩放）
tests/haul_logic.rs  无窗口的 bevy_ecs 集成测试（领取互斥/取消/满仓/…）
assets/art/      运行时加载的 PNG（缺失时自动退化为色块占位）
```

## 美术管线

`art_raw/`（Codex image generation 生成，洋红底）→ `cargo run --bin prep_art`
→ `assets/art/`（透明背景 256×256）。游戏启动时若文件存在则加载，
否则用程序化色块，保证仓库在任何状态下都能跑。

状态：**Playable Slice 0 完成，等待试玩反馈。**
交付报告见 `REPORT.md`，试玩指南见 `PLAYTEST.md`。
