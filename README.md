# Ship Alive — Playable Slice 8 / Settings & Localization (中英双语)

> **Slice 8 新增：设置页面与中英双语支持——[O] 打开设置面板切换语言，
> 全部玩家可见文本（HUD/检视面板/WORK 面板/悬停卡/房间标注/事件日志）
> 即时切换，系统 CJK 字体自动加载，选择持久化到 settings.ini。**

> **Slice 7：环世界式工作优先级列表——WORK 面板（[Tab] 打开），
> 三类工作 × 每名船员的 H/N/L/— 优先级矩阵，点击循环改档，
> 空闲船员即时响应，运行中的任务绝不打断。**

> **Slice 6：玩家可以主动、空间化、守恒地搬运、储存和控制气体——
> 风管层 + 通风口 + 鼓风机 + 储气罐。**

> **Slice 5：空气成为真实、逐格、守恒、可以流动和流失的资源。**

> **Slice 4：门成为真实动态设备；墙和门把船内空间分成气密舱室。**

> **Slice 3：热量成为真实、守恒、空间分布、可运输的量。**

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
  Fabricator 断电停机/复电安全恢复、断电反馈（NO POWER 文字 + 暗环）
- **Slice 3 — 热与冷却**：热量是守恒量（唯一出口 = 散热器向太空排放）；
  逐格空气/墙体温度（温度≠热量，显式热容）；设备发热跟随负载（反应堆
  48+480H/s、制造机 24H/s、泵 4H/s）；热沿空气快、穿墙极慢（船壳绝热），
  局部热点几分钟内成形；反应堆 Overheat 降额 60%（核心受损附加热随超温幅度
  增长，保证断冷却必然升级）、Critical 应急电力（仅够泵运行，防死锁），
  滞迟恢复不抖动；地下冷却液层（与电缆独立）：有限水量按 (水量,温度)
  包沿真实拓扑环路推进，泵断电即滞止；热交换器 空气→水（30°C 阀值，双向
  只往平衡方向）、散热器 水→太空（15°C 旁通、每台 900H/s 硬上限、必须贴船壳）；
  预装稳定环路（1 泵+1 换热器+2 散热器+1 水箱，80% 预充）；拆管保水（挤回
  邻管/水箱，满了才诚实溢出）；Thermal/Coolant 覆盖层（`P` 循环 Off→Power→
  Thermal→Coolant）；SHIP STATUS 热能块、反应堆/泵/水箱/换热器/散热器选中
  面板、常显热警告；BUILD→Thermal 分类（Pipe 拖拽铺设同电缆）
- **Slice 4 — 气密舱室与门**：门有真实运行状态（Closed/Opening/Open/Closing，
  Auto/Hold Open/Lock Closed 三种模式，全程模拟时间驱动）；结构舱室
  （Structural Compartment）是从 Hull/Wall 几何派生的缓存（仅在几何变化时
  重建），门是舱室之间的 portal；当前气密连通（开门即并组、关门即隔离）
  与结构分区分离——门开关只做 portal 图 union-find，绝不全图 flood-fill；
  热服从同一边界（关门=1.2H/K/s 缓慢渗热、开门=22 快速混合，切换严格
  守恒）；Lock Closed 对寻路是墙、船员等门不算拥堵（避让时钟冻结）、
  绝不夹人；门必须建在一格墙口（自动推导 N-S/E-W 朝向，模糊位置拒绝）；
  预装 5 扇 Auto 门（开局 7 个全密封舱室）；Compartments 覆盖层（`P` 第 4
  档：舱室稳定着色、关门红/开门绿、贴太空区域 EXPOSED 警告、悬停高亮整
  舱室）；SHIP STATUS 舱室摘要 + 门选中面板（状态/朝向/两侧舱室/模式按钮）
- **Slice 5 — 大气与压强**：逐格四气体（O₂/惰性/CO₂/污染物，数量为权威
  数据）；压强是从 (气体量, 温度) 派生的理想气体关系（标准舱 ~101.3 kPa /
  O₂ 分压 ~21 kPa）；压力驱动 bulk flow（等压钳位防过冲、全组分随流、
  气体携带显热平流）+ 缓慢组分扩散；门缝语义复用 Airtight 边界（关门完全
  阻断、全开门两侧交换、门格自带真实气体体积）；贴太空破口逐格泄压
  （破口先失压、压力波向内传播、泄出气体带走热量并按 species 记账）；
  气体热容量跟随真实气量（真空≈0，设备热质量保留）；睡眠/唤醒活动模型
  （稳定船近零开销）；Atmosphere 覆盖层（`P` 第 5 档：压强色带 + 成分危险
  警示 + 悬停逐格气体卡）；SHIP STATUS 大气块 + 常显 ATMOSPHERE LOSS 警告
- **Slice 6 — 通风与气体搬运**：地下风管层（DuctGrid，与电缆/水管同级的
  第三个稠密层，每格有限容积 DUCT_MOL=10、真实四组分混合 + 热能）；相邻格
  局部平衡钳位输送（绝不做全网平均——压力波逐格传播）；通风口 Vent
  （Supply 只供气 / Exhaust 只抽气 / Balanced 压差驱动，Open/Closed 阀，
  只与自己头顶那格房间交换）；鼓风机 Blower（压头 15 kPa、上限 12 mol/s、
  断电/停机退化为被动风管格，供电 4 PU）；储气罐 GasTank（400 mol 容积、
  真实混合气、派生压强、阀门开关）；全部 species + 热能守恒——包括拆除
  （风管拆除断链时气体挤回邻管/房间、兜底记入泄出账本；储气罐拆除优先
  排入下方风管否则进房间）；通风绝不合并气密舱室（门关着就过不去人，
  但风管可以绕过锁门送气）；破口可以抽干整张管网；玩家四重隔离手段
  （关通风口/停鼓风机/关罐阀/锁门）；拓扑缓存（只在风管/设备集变化时
  flood-fill，流量步零重建）；睡眠/唤醒（均衡管网静止休眠）；
  Ventilation 覆盖层（`P` 第 6 档：管网压强着色 + 青色流向箭头 + 设备
  状态标记）；SHIP STATUS 通风块；Vent/Blower/Tank 选中面板（模式/
  方向/阀门按钮）；BUILD→Atmosphere 分类；开局预装 FABRICATION↔CREW
  QUARTERS 起步管网（19 格风管 + 2 通风口 + 1 鼓风机 + 1 预充储气罐，
  鼓风机待机不打扰开局环境）
- **Slice 7 — 工作优先级列表（WORK Tab）**：`Tab`（或顶栏 Work [Tab] 按钮）
  打开环世界式优先级矩阵——行 = 工作类型（Haul 搬运 / Build 建造与拆除 /
  Operate 操作制造机），列 = 船员；每格显示当前档位（H 绿 / N 白 / L 灰 /
  — 红关），点击循环 Off→Low→Normal→High→Off；Current 行实时显示每名
  船员正在做什么（含空闲原因），Done 行显示终身 h/b/o 计数；点击列头名字
  选中该船员；Defaults 一键重置全员（并唤醒空闲扫描）；**改优先级立即
  唤醒空闲船员**（不等 nothing-to-do 退避），但**绝不打断进行中的任务**
  （优先级只决定"下一个"任务）；优先级档位压倒距离（High 远任务胜过
  Normal 近任务，档内才比距离）；Esc 先关面板；选中船员面板的 12 个
  优先级按钮移除，统一入口 WORK 面板
- **Slice 8 — 设置与多语言（中/英）**：`loc.rs` 双语字符串表（编译器强制
  两语言字段一一对应；`fmt_` 模板的 `{占位符}` 集合有单测钉死）；
  `settings.rs` 设置面板（`O` / 顶栏 Settings [O] / Esc 关闭）——语言
  English/中文 即点即切（`SetLang` 动作）：动态行按刷新周期跟随，静态
  标题/按钮走 `StaticLabel` 同步系统，房间标注（Text2d）随语言重写；
  事件日志旧条目保留原语言、新条目跟随；语言选择持久化 `settings.ini`
  （exe 目录；`SLICE8_LANG` 强制覆盖，`LANG/LC_ALL` 含 zh 自动中文，
  否则英文）；系统 CJK 字体自动加载（Windows msyh/simhei、Linux Noto、
  macOS PingFang；找到即应用到全部文本——中文字体自带拉丁字形；找不到
  则退回默认字体并在控制台提示）；autotest 控制台输出保持英文基线

```bash
cargo run                      # 启动游戏（玩法见 PLAYTEST.md）
cargo test                     # 205 个单元/集成测试
SLICE0_SCENARIO=A cargo run    # Slice 0/1 验收场景（A–L、P1/P2/M）
SLICE2_SCENARIO=A cargo run    # Slice 2 电力验收场景（A–J、PW）
SLICE3_SCENARIO=A cargo run    # Slice 3 热能验收场景（A/B/C/E/F/R + V 截图）
SLICE4_SCENARIO=A cargo run    # Slice 4 气密验收场景（A–F、H–L、N–P）
SLICE5_SCENARIO=A cargo run    # Slice 5 大气验收场景（A–I、O/P/Q）
SLICE6_SCENARIO=A cargo run    # Slice 6 通风验收场景（A B C D E F G H I L M N P Q R S T U）
SLICE7_SCENARIO=A cargo run    # Slice 7 优先级验收场景（A 专职分工 / B 即时唤醒 /
                               # C 高档压倒距离 / D 任务中禁用不打断 / E 全员停工 /
                               # F 暂停冻结+恢复，配 SLICE0_SPEED=0）
SLICE6_VIEW=ventilation cargo run  # 直接以 Ventilation 覆盖层启动
SLICE7_VIEW=work cargo run     # 直接以 WORK 面板打开启动（截图/检查用）
SLICE8_SCENARIO=A cargo run    # Slice 8 语言验收场景（A 启动状态/字体/持久化 /
                               # B 切中文+落盘 / C 切回英文 / D 文件与活语言一致）
SLICE8_LANG=zh cargo run       # 强制语言（覆盖 settings.ini 与探测）
SLICE8_VIEW=settings cargo run # 直接以设置面板打开启动（截图/检查用）
SLICE5_TOOLS=1 cargo run       # 开发者大气工具：悬停 + F5 破口 / F6 降压 / F7 恢复标准 / F8 注 CO2 / F9 注污染物
SLICE4_DOORPIN=6,6:0.5 cargo run  # 门美术检查：钉住 (x,y) 门的开启进度/模式（[:Auto|HoldOpen|LockClosed]）
SLICE4_DEBUG_DOOR=11,10 cargo run # 门美术检查：绕过建造规则在该格生成一扇门（可放进竖墙验证 Ew 朝向）
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
  thermal.rs     热网格：逐格空气/墙体温度、守恒传导(防过冲钳制)、设备热注入、
                 热状态机(Overheat/Critical+滞迟)、唤醒/休眠活跃集、反应堆降额
  coolant.rs     冷却液层：PipeGrid/WaterGrid(水量+温度包)、DFS 环路推进(满环位移流)、
                 热交换器/散热器交换、水守恒(拆管重分配/诚实溢出)
  production.rs  Fabricator：配方(2 Ore→1 Part)/订单(Produce N|Repeat)/状态机/缓冲
  jobs.rs        核心：玩家动作、四类任务执行、统一工作扫描与优先级领取
  airtight.rs    Slice 4：门运行时（Auto/HoldOpen/LockClosed 状态机、通行需求、
                 防夹人）、结构舱室派生缓存（flood-fill+portal）、气密连通
                 union-find、统一环境边界 boundary() 查询
  atmosphere.rs  Slice 5：逐格四气体网格（数量权威、SoA 布局）、压强/分压派生、
                 等压钳位 bulk flow（全组分+显热平流）、组分扩散、破口泄压
                 （真空边界+泄出记账）、气体热容量同步、睡眠/唤醒活跃集、
                 共享气体原语（pressure_vol/eq_amount/move_gas）
  ventilation.rs Slice 6：地下风管层 DuctGrid（每格有限容积+真实混合气+热能）、
                 相邻局部平衡输送（无全网平均）、Vent（Supply/Exhaust/
                 Balanced+开关）、Blower（压头模型 15kPa/12mol/s、断电退化
                 被动）、GasTank（400mol/阀门/派生压强）、拆除守恒释放规则、
                 拓扑缓存（变化才 flood-fill）、睡眠/唤醒、流向遥测
  movement.rs    逐格移动 + 软避让（含对头死锁的按格累计穿越机制；
                 等门冻结避让时钟）
  simtime.rs     统一模拟时钟（i64 µs、T+HHH:MM:SS、固定步长泵/累加器/退避）
  time_ctrl.rs   玩家倍率 Pause/1×/2×/4×（Space 记忆上次倍率）
  input.rs       选择/框选/建造工具/ghost/拆除点击/快捷键/相机
  render.rs      建筑/蓝图/机器状态可视化 + 放置 ghost + 房间标注
  ui.rs          HUD（环世界式四角浮动面板）：左上状态+事件流、右上船钟+速度+警报、左下 BUILD 分类栏、中下船员条、右下检视面板
  loc.rs         Slice 8：Lang 资源 + 双语字符串表（EN/ZH 同字段集，编译器强制
                 覆盖）+ 领域枚举本地化访问器 + tfmt! 运行时模板宏 + 占位符
                 对齐单测
  settings.rs    Slice 8：设置面板（语言切换/持久化 settings.ini）、UiFont
                 （系统 CJK 字体探测与应用）、StaticLabel 静态文本语言同步
  worktab.rs     Slice 7：WORK 面板（优先级矩阵池、档位循环按钮、Current
                 活动行、点击列头选人、Defaults 重置、[Tab]/Esc/顶栏开关）
  ui_overlay.rs  悬停 tooltip + 框选矩形
  autotest.rs    SLICE0_SCENARIO=A..L + P1/P2/M 自动验收与试玩驱动器（开发工具）
  stats.rs       开发遥测（产量/搬运距离等，用于布局 A/B 对比）
  setup.rs       从字符画生成世界（含预置 Fabricator 与 P/O 库存货架）
  bin/prep_art.rs 生成美术的后处理（去背/裁切/缩放）
tests/
  path8.rs       8 向移动测试（对角速度一致/混合步/禁穿角/避让）
  airtight.rs    Slice 4 气密测试（舱室/暴露/门户/连通/门模式/防夹/等门/
                 禁穿锁门/热隔离与守恒/缓存/128×128 性能）
  haul_logic.rs  Slice 0B 无头集成测试（领取互斥/框选/满仓/…）
  ship_ops.rs    Slice 1 无头集成测试（建造/拆除/生产/过滤/优先级）+
                 Slice 7 优先级语义测试（循环次序/即时唤醒/任务中禁用不打断/
                 全禁用原因/高档压距离/Defaults 重置唤醒）
  power_ops.rs   Slice 2 电力测试（拓扑/分割/并网/过载/断电生产）
  fleet_ops.rs   多制造机建造+材料守恒+封死房间不泵料
  thermal.rs     热能集成测试：守恒/启动稳定/满载危机+恢复/滞迟/暂停不变性/
                 泵断电滞止/网络分合保水/散热上限/128×128 性能
  atmosphere.rs  Slice 5 大气测试（守恒/分压/破口/门交换/热平流/睡眠/性能）
  ventilation.rs Slice 6 通风测试（有限容积/局部性/模式语义/鼓风机压头与上限/
                 罐混合与阀门/拆除守恒/破口抽管网/拓扑缓存/独立网络/暂停与
                 倍速等价/睡眠/128×128 稳定与活跃性能）
  localization.rs Slice 8 本地化测试
  perf_jobs.rs    性能第二轮基准（A* 进程内 A/B 对照、满载工作扫描、
                 128×128 全链搬运 churn；release 跑 --nocapture 看数字）（settings.ini 往返/损坏回退/文件格式/
                 系统 CJK 字体解析（无则跳过）/关键表项双语覆盖锚点）
tools/           截图脚本（开发用）
assets/art/      运行时加载的 PNG（缺失时自动退化为色块占位）
art_raw/         Codex image generation 原图（洋红底）
```

## 美术管线

`art_raw/`（Codex image generation 生成，洋红底）→ `cargo run --bin prep_art`
→ `assets/art/`（透明背景 256×256）。游戏启动时若文件存在则加载，
否则用程序化色块，保证仓库在任何状态下都能跑。

状态：**Slice 8（Settings & Localization）完成，等待试玩反馈。**
交付报告：`REPORT.md` / `REPORT_0B.md` / `REPORT_1.md` / `REPORT_2.md` /
`REPORT_PATH_8WAY.md` / `REPORT_TIME.md` / `REPORT_THERMAL.md` /
`REPORT_ATMOSPHERE.md` / `REPORT_VENTILATION.md` / `REPORT_PRIORITIES.md` /
`REPORT_LOCALIZATION.md`；
性能优化轮：`REPORT_PERF.md` / `REPORT_PERF2.md`；
试玩指南：`PLAYTEST.md`；代理经验：`AGENTS.md`。
