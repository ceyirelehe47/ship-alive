# Ship Alive — 试玩指南（Slice 0 / 0B / 1）

## 启动

```bash
cargo run            # 开发模式即可流畅运行
cargo run --release  # 更流畅
```

窗口 1440×860。整艘 Starter Ship（36×19 格）开局就在视野内，
房间标注：CARGO HOLD / CREW QUARTERS / ORE BAY / PARTS ROOM / **FABRICATION**（有预置
Fabricator）/ STORAGE（淡琥珀色高亮）。

## 基础操作（全部可通过鼠标完成）

| 操作 | 效果 |
| --- | --- |
| 左键点击船员 / 物品 / 货架 / 建筑 / 蓝图 | 选中，左下面板显示详情与操作按钮 |
| 左键点击空地 | 取消选中 |
| **左键在地图上拖动** | 框选：框内所有物品标记搬运（青色矩形预览） |
| 鼠标悬停 | 白色高亮环 + 跟随光标的信息卡 |
| 右键拖动 / `WASD` / 方向键 | 平移镜头 |
| 滚轮 | 平滑缩放 |
| `T` | 标记/取消选中物品的搬运 |
| `H` | 全部标记搬运 |
| `C` | 取消全部搬运（携带中的物品会被放下） |
| `B` | 循环切换建造工具（墙→门→货架→制造机→拆除→关闭） |
| `Esc` | 关闭建造工具 / 取消选中 |
| `Space` / `1` / `2` / `3` | 暂停 / 1× / 2× / 4× |

## Slice 1 新玩法：经营你的船

### 建造（BUILD 栏）

顶栏 **BUILD:** 按钮组按**分类**收纳：`Structure`（Wall/Door）、`Storage`（Storage Rack）、
`Machines`（Fabricator/Reactor）、`Power`（Power Cable），外加 `Deconstruct` 与
`Cancel Tool`。**点击分类展开浮窗**，再点具体建筑选中工具（浮窗自动收起）；
正在使用的工具所在分类保持高亮。选中工具后：

- 鼠标处出现放置虚影，**绿色** = 可放置，**红色** = 不可放置（悬停文本显示原因）。
- 左键放置蓝图。蓝图**不会立即成型**：
  1. 自动物流从最近的未标记地面零件或货架库存调零件到工地；
  2. 零件到齐后一名船员前往施工数秒；
  3. 建筑完成，墙/制造机所在格立即影响寻路。
- 选中蓝图后左下面板可 **Cancel blueprint（退还已送材料）**。
- **Deconstruct 工具**：点击建筑标记拆除（黄色高亮），船员执行后**全额返还建材**
  （拆货架同时掉落内存货物）。再点一次取消标记。

### 生产（选中 Fabricator）

- 配方：**2 Asteroid Ore → 1 Machinery Part（6 秒）**。
- 面板按钮：`+1 batch`、`+5`、`Repeat`（无限循环）、`Clear order`。
- 机器状态一目了然（机器上方文字 + 光环颜色）：
  - 灰 `no order` / 橙 `need 2 ore`（等供料）/ 蓝 `waiting for worker` /
    绿 `working N%` / 红 `output blocked`（产出堆满，搬走即恢复）。
- 自动物流：订单缺矿 → 自动从矿石架/地面拉矿入机器；产出 → 自动搬到接收零件的货架。

### 仓储过滤（选中货架）

- 面板显示容量、库存、以及 **allow/deny** 按钮切换 Crate/Ore/Part。
- 配合距离可以主动设计物流：
  **矿石架贴着 Fabricator 放、零件架守着出料口**，搬运距离骤减。

### 工作优先级（选中船员）

- 每人 Haul / Build / Operate 三类工作独立设置 Off / Low / Normal / High
  （左下面板 12 个按钮，当前档高亮）。
- 玩家可以表达"Ava 专搬箱子、Rex 专搞建造、Mio 看机器"。
- 同档内按距离就近领活；高档显著优先。

## 怎么看懂正在发生什么

- **物品**：无标记 = 躺在地上；琥珀环 = 已标记入库；环变成船员色 = 已被领取；
  物品消失 + 船员头顶小图标 = 携带中；红环 = 不可达。
- **船员**：头顶名字；空闲变暗；底部状态条显示每人正在做什么、为什么停。
- **蓝图**：半透明蓝色 + 材料计数（`part 0/1`）；施工中显示百分比。
- **Fabricator**：状态光环（见上）+ 输入/产出缓冲计数。
- **顶栏**：`Parts made` / `Built` 累计、仓储总量（满仓红字）。
- **事件日志**（右下）：领取/送达/生产/失败全程记录，失败为红色。

## 推荐试玩流程（10 分钟体验 Slice 1 闭环）

1. 选中最下方 FABRICATION 房间里的 Fabricator，点 `Repeat` 开始生产。
   观察船员自动从右下角矿石架拉矿、操作机器、把产出的零件搬回仓储。
2. 感受到"矿架太远了"之后：暂停，用 BUILD 栏在 Fabricator 旁边建两个 Rack。
3. 把新 Rack 过滤设为 **只收 Ore**；把其他所有 Rack 的 Ore 设为 deny。
4. 拆掉右下角的两个旧 Ore 架（O 字样那两个），`H` 全标，让矿石回流到新架。
5. 恢复 4× 速度，对比生产节奏 —— 供料搬运距离大幅缩短，吞吐明显提高。
6. 再试试给船员分工（选中船员调 Haul/Build/Operate 优先级），观察领取变化。

## 验收场景步骤（正常 UI 即可完成）

### A–F（Slice 0B 回归）

- **A 正常搬运**：拖框/Haul All → 全员搬运入库（密封舱 1 件除外）。
- **B 仓储已满**：Debug 展开 → `+Crate` 超容量 → FULL、船员显示 no free storage space。
- **C 不可达物品**：全标后左下 PARTS ROOM 密封小格内货箱红环 unreachable。
- **D 多人竞争**：开局 Haul All → 四人领取四个不同目标。
- **E 目标失效**：Debug `X` 删除搬运中目标 → 任务安全取消并领取新工作。
- **F 时间加速**：`H` + `4×` + 加压 → 行为稳定。

### G–L（Slice 1）

- **G 建造**：BUILD 栏放 Rack → 材料自动送达 → 施工完成。
- **H 拆除**：Deconstruct 标记一个货架 → 船员拆除 → 建材返还、预约清理。
- **I 生产**：选 Fabricator 下单 → 供矿 → 操作 → 零件产出并入库。
- **J 仓储过滤**：两个 Rack 分别只收 Ore / Part → 不同物品各归其位。
- **K 工作竞争**：设置专职工人（hauler/builder/operator）→ 分工清晰可见。
- **L 4× 压力**：多蓝图 + 订单 + 大量搬运同时进行 → 无重复任务/无卡死。

```bash
SLICE0_SCENARIO=A cargo run   # …依次到 L；输出 SCENARIO_RESULT 摘要后自动退出
SLICE0_SCENARIO=P1 cargo run  # 试玩1脚本：建 Rack/墙/门、取消蓝图、拆除重建
SLICE0_SCENARIO=P2 cargo run  # 试玩2脚本：配置原料/成品架 + 生产订单
SLICE0_SCENARIO=M cargo run   # 试玩3脚本：差布局 → 改造（近距矿架）→ 吞吐对比
SLICE2_SCENARIO=A..J cargo run  # 电力验收：健康网/断线/分割/重接/停堆/过载/建/拆/倍速/回归
SLICE2_SCENARIO=PW cargo run   # 电力试玩：开视图→剪线→复电全程
```

## Slice 2 新玩法：电力（Power）

- **开局即通电**：FABRICATION 左下角的 Starter Reactor（绿环、`reactor 100 PU`）
  通过预铺电缆给初始 Fabricator 供电。
- **`P` / 顶栏 Power 按钮**开关电力视图：电缆按网络着色（无源网络暗红）、
  设备周围供电状态环（绿=有电 / 红=无源 / 黄=过载 / 灰=未接线）、
  顶栏网络摘要 `POWER | NET 1: gen 100 dem 20 … headroom 80`。
- **建造电力设施**：BUILD 栏 `Power Cable`（选中后**按住左键拖画**整条线，绿/红 ghost），
  `Reactor`（2×2、8 零件）。电缆走地板下：不占地、可穿内墙/机器/门，只有船壳边框禁铺。
- **拆除**：Deconstruct 工具点电缆格（电力视图下最直观）→ 黄框 → 船员 1 秒拆除。
- **断电表现**：Fabricator 头顶 `NO POWER — no cable/no generator/power shortage` + 红环；
  选中面板显示原因与所在网络状态；恢复供电后自动安全复产（材料不丢不复制）。
- **过载**：同一网络需求 > 发电时**最新设备先被卸载**（先建先得，无随机抖动）。
  一台 Reactor 带 5 台满载 Fabricator；第 6 台起需要分网或加堆。
- **封门警告**：把 2×2 机器建在房间唯一门口会静默封死整个房间
  （里面的蓝图将无人供料 — 这是真实拓扑行为，拆掉机器即恢复）。

## 开发者工具

- 顶栏 **Debug** 开关（默认折叠）：+Crate / +Ore / +Part 生成、`X` 删除选中物品。
- `SLICE0_TRACE=1 SLICE0_SCENARIO=... cargo run`：每 2 秒打印船员轨迹。
- `SLICE0_SCAN_DEBUG=1 ...`：打印工作扫描/任务内部状态（诊断用）。
- `SLICE0_SHOT=<frame>[:<path>] cargo run`：引擎内截图（无窗口抓屏的可靠替代）。
- `cargo test`：64 个单元/集成测试。
