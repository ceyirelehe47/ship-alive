# Ship Alive — Playable Slice 8 交付报告：设置页面与多语言（中/英）

> 目标（用户简报）：**"设置页面与多语言支持和切换（目前先只支持中英两语言）"**。
> 交付：设置面板（语言切换 + 持久化）、全量玩家可见文本的中英双语化
> （HUD / 检视面板 / WORK 面板 / 悬停卡 / 地图房间标注 / 事件日志 /
> 建造 ghost / 覆盖层摘要与警报）、系统 CJK 字体自动加载。

## Summary

- **设置面板**（`src/settings.rs`）：`O` 键 / 顶栏 **Settings [O]** 按钮打开，
  `Esc` 先关设置（其次 WORK、再退工具）。当前设置项：**语言** ——
  English / 中文 两个选项按钮（当前项高亮），切换即 `Action::SetLang`。
- **双语表**（`src/loc.rs`，~700 行）：`Lang` 资源 + `Strings` 表结构 ×
  `EN`/`ZH` 两个 const 实例 —— **字段集编译期强制一致**（漏译直接编译失败）；
  领域枚举（建筑/物品/工作类型/门模式/通风模式/方向/电力/热/机器/冷却/
  覆盖层/房间名/放置错误/空闲原因/警报…）的本地化访问器集中在 loc.rs；
  英语 `label()`/`summary()` 系列保留为控制台/测试基线。
- **运行时模板**：Rust `format!` 只接受字面量模板，因此新增 `tfmt!` 宏
  （`{key}` 占位符逐个 replace）。约定：表内 `fmt_` 前缀字段只用**命名**
  占位符，且全部登记进 `format_pairs()`；单测断言 EN/ZH 占位符集合一致
  ——本 slice 它当场抓到一个真实 bug（zh 的 `{do}` 没跟着 en 改成
  `{doors_open}`，`do` 是 Rust 关键字不能作命名参数）。
- **静态文本同步**：`StaticLabel(Box<dyn Fn(&Strings) -> &str>)` 组件 +
  每帧比对写入的同步系统；按钮标题/面板标题/提示行/BUILD 分类（捕获
  kind 的闭包）全部走它，语言切换无需重启。动态行本来就按刷新周期
  重写，自然跟随。
- **持久化**：`settings.ini`（exe 目录，`lang=zh|en`）保存即写；
  解析顺序 `SLICE8_LANG` 覆盖 → 文件 → `LANG/LC_ALL` 含 zh → 英文。
  纯函数 `load_lang_from`/`save_lang_to` 可单测。
- **CJK 字体**：`UiFont(FromWorld)` 按候选表探测系统字体（Windows
  msyh.ttc → simhei.ttf → simsun；Linux Noto；macOS PingFang），
  `Font::try_from_bytes` 解析后应用到**全部**文本（CJK 字体自带拉丁
  字形，两种语言同一字体）；找不到退回默认字体并打印提示。本机实测
  微软雅黑 TTC 解析成功，中文渲染无豆腐块（截图视觉核验）。
- autotest/控制台输出**保持英文**（既有验收基线不动）。

## 覆盖面（翻译了什么）

| 层 | 位置 | 处理 |
| --- | --- | --- |
| HUD 骨架 | ui.rs build_hud | 静态标题/按钮 → `klabel`/`kbutton`（StaticLabel）；速度"暂停"、View [P]、设置/工作按钮 |
| 顶栏动态行 | hud_update / overlay_cycle | 统计行、舰船时间、船员条（含 task_label）、View 按钮文字 → tfmt |
| SHIP STATUS | sidebar_system | 电力/热/冷却/舱室/大气/通风/仓储/生产 全部行 → tfmt + loc 访问器 |
| 覆盖层摘要+警报 | overlay_summary | 6 种视图摘要行 + 反应堆/制造机/大气/鼓风机/储罐警报 → 表字段；`starts_with("ATMOSPHERE LOSS")` 判断改为读原始计数 |
| 检视面板 | selection_panel_system | 船员/物品/货架/蓝图/反应堆/门/通风口/鼓风机/储罐/通用建筑/制造机 全部分支 + 全部操作按钮标签 |
| 悬停卡 | ui_overlay.rs | 实体卡 + 大气逐格卡（真空/成分/舱室号） |
| WORK 面板 | worktab.rs | 标题/行名/说明/按钮/提示 → wlabel+StaticLabel；sig 增加 lang 维度 |
| 地图标注 | render.rs | 6 个房间名（`RoomLabel` 标记 + 切换重写系统）、EXPOSED/VENTING TO SPACE、制造机/反应堆状态牌、放置 ghost（建筑名+零件数/放置错误/拆除） |
| 事件日志 | jobs/building/time_ctrl/airtight/ventilation | 全部 ~90 条日志（领取/失败原因/施工/拆除/铺设/门模式/设备操作/速度/语言）→ tfmt；`l` 经辅助函数线程传递 |
| 数字/单位 | — | O2/CO2/kPa/PU/H/s、1x/2x/4x、百分号、坐标格式保持原样 |

## 验收结果（SLICE8_SCENARIO，2026-08-17 实机）

| 场景 | 结果 |
| --- | --- |
| A 启动状态 | `lang=en font=loaded cjk_chars_ui=2（设置面板的"中文"按钮）cjk_chars_world=0 settings_file=target\debug\settings.ini [<none>]`；英文样例行正常 |
| B 切中文 | 发 SetLang(Zh)+MarkAll 后：`cjk_chars_ui=520 cjk_chars_world=23`（房间标注全中文）；样例 `已标记: 24 | 仓储: 16/40 | 零件 0 | 已建 0 | 空闲 0/4`（占位符全解析）；`settings.ini` 落盘 `[lang=zh]` |
| C 切回英文 | 中途 zh_cjk=468；切回后 `lang=en`、样例行恢复英文；残留 6 个 CJK 字符 = 设置面板按钮"中文" + 历史日志条目（设计如此） |
| D 文件一致 | 无覆盖启动（读上一步写盘的 en）：`live=en file=[lang=en] match=true`（文件解析路径同时得到验证） |

## Playtests（实机截图 + 视觉模型核验）

1. **PT1 英文**（`SLICE8_LANG=en`）：HUD/侧栏/地图标注全英文，字体正常。
2. **PT2 中文**（`SLICE8_LANG=zh`）：**汉字全部正常渲染（无豆腐块）**——
   雅黑生效；顶栏统计、右下检视、地图房间名（货舱/船员舱等）可读；
   未见未翻译的英文 UI 残留（单位/化学符号除外）；布局无重叠。
3. **PT3 设置面板**（`SLICE8_VIEW=settings` + zh）：面板居中——标题
   "设置"、语言节、English/中文 两按钮（**中文高亮**）、说明行、
   关闭 [O]；面板外 UI 为中文。

## 测试（202 = 197 + 5 新增）

- `src/loc.rs` 单测 ×4：占位符对齐（抓到 `{do}` bug）、登记锚点、
  Lang 编解码往返、领域标签双语差异。
- `tests/localization.rs` ×5：settings.ini 往返（两语言）、缺失/损坏
  回退默认、文件格式稳定（`lang=zh\n`）、系统 CJK 字体解析（无字体
  机器自动跳过）、关键表项双语覆盖锚点。
- 既有 5 个测试世界的 Harness 补插 `Lang` 资源（jobs 系列系统新增参数）。

## Design assumptions

- **表驱动 const 结构体**（编译期双语覆盖保证）而非运行时文件/
  fluent——零依赖、可 grep、漏译编译失败；代价是加字符串要动一处表。
- **tfmt! 运行时模板**：`format!` 字面量限制的直接解法；放弃编译期
  占位符检查，用登记表 + 对齐单测补位。
- **英语为控制台/测试基线**：autotest 打印与英文 `label()` 系列不动，
  玩家界面走 loc。旧日志条目不追改（与 RimWorld/ONI 行为一致）。
- **系统字体而非捆绑字体**：Windows/Linux/macOS 都带 CJK 字体；
  雅黑 TTC 实测可被 bevy(cosmic-text) 解析。零仓库体积代价。
- 语言解析优先级：`SLICE8_LANG`（调试强制）→ settings.ini →
  `LANG/LC_ALL` 含 zh → En。

## Temporary behaviors

- `SLICE8_LANG` / `SLICE8_VIEW` 为开发钩子（验收/截图用）。
- 制造机面板里 "POWER: … — machine halted" 在源码中本就重复输出两次
  （Slice 3 遗留），翻译保持了同样的重复，未顺手改行为。
- 调试工具栏（Debug 展开行）与控制台输出保持英文。

## Known issues

- 事件日志 RingBuffer 存的是最终字符串：切换语言只影响新条目（设计）。
- `StaticLabel` 同步系统每帧全量比对（几十个文本，写前比对，开销可忽略）。
- 极旧 Linux 发行版无 Noto CJK 时中文退化为方框（控制台有 UIFONT 提示）；
  正式发版时应捆绑开源 CJK 字体（见 Deferred）。
- `settings.ini` 在 exe 目录：`cargo run` 下即 `target/debug/`，换构建
  目录不共享（单人开发机可接受）。

## Deferred（本 slice 不做）

- 更多语言（日/俄/…）：加 `Lang` 变体 + 一张表即可，架构已就绪。
- 捆绑开源 CJK 字体（Noto Sans SC）替代系统字体探测（发版时做）。
- 字体大小/界面缩放设置、音量、键位重映射等其他设置项。
- 数字/日期本地化格式（千分位等）——当前固定英文数字格式。

## Git

- 代码与文档提交：**`be03bad`**（main，直接提交，未 force-push）。
- 门禁：`cargo fmt` ✅、`cargo clippy --all-targets --all-features -- -D warnings` ✅、
  `cargo test` **202/202** ✅；全量回归批（S0-A/K、S2-A、S3-A、S4-A、
  S5-A、S6-A、S7-A/F）复跑通过。
