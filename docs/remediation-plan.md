# DesktopPet · 最小可行修复方案设计（不含代码实现）

- 作者：架构师 高见远
- 前置：`docs/system_design.md`（根因诊断报告）；主理人交叉验证结果
- 约束：只做设计；可读代码、可写分析脚本到 `scripts_tmp/`

---

## 0. 我独立复核后对主理人数据的**修正**（重要）

我写了独立 PNG 手解探针 `scripts_tmp/_arch_atlas_probe.py` / `_arch_atlas_geom.py`，结果**支持主理人的核心结论，但有一处关键几何数据需要修正**：

| 项 | 主理人数据 | 我的实测 | 判定 |
|----|-----------|---------|------|
| 29 张图集颜色数 | 恒 3 色 | 恒 **4 色**（主色/`#c8c8c8`/`#ffffff`/`#000000` 透明） | ⚠️ 差 1（探针把透明算作一色） |
| 主色 | E红/I绿/M蓝/T黄 | 一致：`#ea4335`/`#34a853`/`#4285f4`/`#fbbc05` | ✅ |
| 色块尺寸 | 64×64 | ✅ `px=4096=64²`，`bbox` 尺寸恒 `(64,64)` | ✅ |
| 非透明占比 | 0.0673~0.0684 | ✅ 完全一致 | ✅ |
| **帧尺寸** | 未提及 | ⚠️ **`frameW=frameH=256`**（`atlas.json`），图集为 `1024/1536/2048 × 256` | 关键 |
| **"8 帧完全相同"** | 是 | ❌ **不相同**！色块**逐帧平移** | 需修正 |

**修正后的精确几何（`ACT-T-01`，2048×256，8 帧）：**

```
frame#0 主色 bbox(帧内)=(48,128,111,191)   → 色块 64×64
frame#1 主色 bbox(帧内)=(69,128,132,191)   → 右移 +21px
frame#2 (89,128,152,191)  frame#3 (110,128,173,191)
frame#4 (130,128,193,191) frame#5 (151,128,214,191)
frame#6 (171,128,234,191) frame#7 (192,128,255,191)  ← 右边缘贴 255
每帧另含：一条 y=248 的 1px 半透明灰线（#c8c8c8, a=120，宽 224）
        一个 y=120~125 的白色小条（#ffffff, a=255），宽度 6→14→22→…→60 递增
```

即：**占位素材是一个"斜向走动 + 白条逐渐变长"的方块动画**，不是静态色块。这一修正**不影响主理人的结论方向**（它依然是零宠物像素的占位物），但对**新生成器的正确性验证**很关键——新图集必须避免"平移量超出帧宽导致跨帧污染"。

**另需注意一个真实隐患**：`frame#7` 的色块 bbox 右边界 = 255（贴帧边界），且色块宽 64 —— 在 256 帧宽内安全。但若未来生成的图形**总跨度 > 256**，`gen-atlas.mjs:336-345` 的切片逻辑会把相邻帧内容切进来。**新生成器必须逐帧独立绘制、显式限制在 `[0,256)` 内**。

---

## 问题 1（核心决策）—— 如何让宠物真正显示出来

### 三条路线对比

| 维度 | 路线 A：程序化生成 | 路线 B：Spine 4.2 + 真资产 | 路线 C：A + 保留 B 框架（推荐） |
|------|------------------|--------------------------|----------------------------|
| 用户可见效果 | **立即可见**（可辨识宠物） | 不可行——**资产不存在，无法凭空获得** | **立即可见** |
| 离线/零网络 C9 | ✅ 纯 `node:zlib`，`gen-atlas.mjs` 已具备 | ⚠️ 需引入 `spine-webgl` 运行时 | ✅ |
| 许可 C4（禁 GPL） | ✅ 自绘无第三方代码 | ⚠️ Spine Runtime License 需法务核（非 GPL 但需授权） | ✅（不引入） |
| 工作量 | 中（写一个生成器） | **极大且被资产阻塞** | 中 |
| 对现有代码破坏性 | **零**（只换 `resources/atlas/` 内容） | 大（替换 `spineAdapter.ts` 占位、加依赖） | 零 |
| 是否偏离原设计 | 部分（原 `prefer:skeleton` 仍不生效） | 符合原意 | 符合（框架保留） |
| 风险 | 程序绘制"不像宠物" | **无法交付** | 同 A |

### 推荐：**路线 C**（同意主理人倾向，但补充关键约束）

**理由**：
1. **B 是死路，不是"难路"**。真实 Spine 资产（`xinyuehu.json`/`.atlas`/`.png`）**从未存在**，不是"待接入"而是"待创作"。在没有美术资源的前提下，接入运行时也画不出东西——投入产出为负。故 B **不可选**。
2. **A 的技术风险可控，前提是采用"数据驱动 + 可验证"的绘制方式**，而非"随手画"：
   - 用 **SVG path / 参数化骨架**定义狐形角色（头/耳/身/尾/四肢），逐帧对骨骼做旋转/位移，再**栅格化到 256×256 RGBA**；
   - 关键是**栅格化必须零依赖**：`gen-atlas.mjs` 已有手写 PNG 编码器（`encodePng`），我们只需补一个**纯 JS 的"骨架 → 像素"栅格器**（画椭圆/贝塞尔描边填充），或更稳妥地**用 SVG 生成后用 Node 内置能力渲染**——但 Node 无内置 SVG 栅格器，故**采用参数化几何直接写像素**（椭圆+三角+描边）是唯一零依赖路径。
   - 为了让"像宠物"，采取**形状组合**而非纯几何：狐耳（双三角）+ 圆头 + 吻部 + 大尾（贝塞尔曲线扇形）+ 眼/鼻点缀；配色用暖橙 + 白腹 + 深色描边（`--pet-outline: #2b2436` 已有该色板）。
3. **C 的"保留 B 框架"成本为零**：`BackendSwitcher` / `SkeletonRenderer` / `spineAdapter.ts` **完全不动**（占位适配器继续让 3s 后定格帧回退，这是**正确的降级行为**）。

### 各路线对现有代码的改动范围

| 路线 | 需改动 | 需新增 |
|------|--------|--------|
| A / C | **`resources/atlas/` 29 张 PNG + `atlas.json`**（由生成器覆盖）；`main.ts` TDZ 2 行；`supervisor.rs` 自愈 3 处 | `scripts/gen-placeholder-pet.mjs`（新生成器） |
| B | `src/renderer/spineAdapter.ts`（替换占位）；`package.json`（加 `spine-webgl`）；`Cargo.toml`（若走 Rust 侧） | Spine 运行时 + **不存在的**美术资产 |

> **架构裁定**：选 C。**同时必须诚实标注**：`character.json.renderer.prefer="skeleton"` 在本方案下**仍不生效**（见问题 4，建议改为 `"frame"` 以消除"死配置"的误导）。

---

## 问题 2 —— 占位图集与真实图集的区分机制

**风险**：路线 A 的产物直接覆盖 `resources/atlas/`，未来真实美术交付后 `gen-atlas.mjs` 运行会再次覆盖 / 或有人误以为占位是正式资产。

**三层防护设计**：

1. **输入目录分离（根因层）**
   - 生成器输入固定为 `assets/sprites/_generated/xinyuehu/{actionId}/{state}/`（**下划线前缀显式标记"程序生成"**）；
   - 真实美术交付到 `assets/sprites/xinyuehu/`（无前缀）；
   - `gen-atlas.mjs --src` 明确指向目录 → 两条管线互不覆盖。

2. **产物来源标注（可追溯层）**
   - 在 `resources/atlas/` 下新增 `SOURCE.json`（**不手写 `atlas.json`**，遵守 `gen-atlas.mjs:12` 禁令）：
     ```json
     { "generated": true, "generator": "scripts/gen-placeholder-pet.mjs",
       "generatedAt": "<ISO8601>", "style": "procedural-fox", "replaces": "placeholder-swatch",
       "note": "程序生成占位形象，非精绘美术。真实美术交付后请删除本文件并重跑 gen-atlas.mjs" }
     ```
   - `.gitignore` 增加：真实美术路径不忽略、`_generated/` 可忽略（或反之，取决于是否希望入库）。

3. **防覆盖守卫（执行层）**
   - `gen-placeholder-pet.mjs` **写入前检查** `resources/atlas/SOURCE.json`：
     - 不存在 或 `generated===true` → 允许覆盖；
     - 存在且 `generated!==true`（即真实美术已交付）→ **拒绝执行并打印指引**，退出码非零。
   - `gen-atlas.mjs` 同样加一道：若目标目录存在 `SOURCE.json` 且 `generated===true`，而输入源是**真实美术目录** → 提示"将覆盖程序生成占位物"后继续（正式管线优先）。

---

## 问题 3 —— 24 个缺失动作怎么办

实测确认（`scripts_tmp/_diag_action_diff.txt`）：`actions.json` 声明 **53** 个，`atlas.json` 实有 **29** 个，缺 **24**：

```
ACT-N-01 ~ ACT-N-16   （16 个，生存/需求系列）
ACT-S-01 ~ ACT-S-04   （4 个，交互系列）
ACT-P-01 ~ ACT-P-04   （4 个，提醒系列）
```

**建议：本次一并生成全部 53 个**（而非保持降级）。理由：

1. **成本几乎为零**：路线 A 是**参数化生成器**——53 与 29 的差别仅为"多跑 24 次同一函数"，不增加复杂度。
2. **消除用户可感的功能缺口**：`pet-err.log` 已实证 `提醒动作 ACT-P-03 未启用（批次 C 资源未交付）→ 触发面就绪，待资源` —— 即**喝水/久坐提醒弹不出来**。这属于"功能不可用"，直接违背本次目标。
3. **消除日志噪声**，让"未启用"告警的语义恢复为真异常。
4. **`hit` 掩码库一致性**：`hit_latest` 掩码库按图集构建（日志 `actions=29 frames=210`），补齐 53 个后掩码覆盖率提升，命中判定在生存/交互动作下也能生效。

**前提**：`actions.json` 里这 24 个动作的 `loopRange` / `fps` / `looping` 字段决定了帧数与速率，生成器必须**读 `actions.json` 驱动生成**（而非硬编码 8 帧），否则 `loopRange` 会越界。

> ⚠️ 顺带修正：`ACT-N-*` 属"生存系列"，需确认其**语义**是否需要特殊姿态（如睡眠/进食）。若 `actions.json` 无足够信息，按 `category` + `name` 生成通用姿态并在 `SOURCE.json` 登记限制。

---

## 问题 4 —— 附带的必修项清单（有序，含依赖）

### 修复任务列表

| ID | 任务 | 文件（具体路径） | 依赖 | 优先级 |
|----|------|----------------|------|--------|
| **T01** | **写参数化宠物生成器**：读 `resources/config/actions.json` 驱动，按 `actionId` 生成 256×256 RGBA 帧（狐形 + 描边 + 透明底），输出到 `assets/sprites/_generated/xinyuehu/` | 🆕 `scripts/gen-placeholder-pet.mjs` | — | **P0** |
| **T02** | **跑生成器 + 重打包图集**：产出 53 组序列帧 → `node scripts/gen-atlas.mjs --src assets/sprites/_generated --out resources/atlas`；同时写 `SOURCE.json` | `assets/sprites/_generated/**`、`resources/atlas/*.png`、`resources/atlas/atlas.json`、🆕 `resources/atlas/SOURCE.json` | T01 | **P0** |
| **T03** | **修 `main.ts` TDZ + 崩溃可观测**：`const menu` 上移到 `menuView` 之前；`bootstrapPetWindow` 包 `try/catch`；`void bootstrapPetWindow()` → `void bootstrapPetWindow().catch(err => console.error('[pet] 引导失败', err))` | `src/main.ts:160`、`:178`、`:287` | — | **P0** |
| **T04** | **修看门狗重启循环**：`self_heal_renderer` 在 `reload()` 后**重置 `FrameWatchdog` 计数 + 重锚 supervisor 观察窗**；`Restart` 分支加**会话内重启次数熔断**（超阈值改为只弹托盘不重启） | `src-tauri/crates/dp-app/src/supervisor.rs:368-378`、`:451-474`；`bridge.rs` 需补 `FrameWatchdog::reset()` | — | **P1** |
| **T05** | **修 clippy 门禁**：`assert!(table.is_empty() == false)` → `assert!(!table.is_empty())` | `src-tauri/crates/dp-assets/src/skelhit.rs:191` | — | **P1** |
| **T06** | **消除"死配置"误导**：`character.json.renderer.prefer` 由 `"skeleton"` 改 `"frame"`；并在 `fallbackChain` 注释/`SOURCE.json` 说明骨架路径未接入。**或**（更彻底）在 `RendererCfg` 上标注"当前无消费点"并补一个**启动期一致性自检**（声明 skeleton 但资产缺失 → 明确日志） | `resources/config/character.json:22-33`、`src-tauri/crates/dp-core/src/config/model.rs:631-670` | T02 | **P1** |
| **T07** | **修硬编码契约字面量**：`gen-skeleton.mjs:5` 与 `skelhit.rs:6` 的 `53 clip` 改为从 `actions.json` 读取或标注"当前 53"；避免与 `actions.json` 漂移 | `scripts/gen-skeleton.mjs:5`、`src-tauri/crates/dp-assets/src/skelhit.rs:6` | T02 | **P2** |
| **T08** | **修菜单几何**：`DomMenuView.containerSize()` 改用**承载容器 `root`** 尺寸（当前用 `rootEl` 自身）；评估 256² 窗内九宫格菜单可用性（必要时改为紧凑两列或允许溢出窗口） | `src/renderer/DomLayers.ts:333-335`、`src/styles/pet.css` | — | **P2** |
| **T09** | **补装配层烟测**（补结构性盲区）：jsdom/Playwright 跑 `bootstrapPetWindow()`，断言①无 throw ②收到 frame 后 `frame_receipt` 被 invoke ③真实 PNG 解出非"单色"像素 | 🆕 `src/__smoke__/bootstrap.smoke.test.ts` | T02,T03 | **P2** |
| **T10** | **修 `dp-app` doctest**：`tray_menu.rs:41` 的 `use tauri::...` 在 doctest 上下文 rlib 缺失 → 改用 `no_run`/`ignore` 或补 doctest 依赖；CI 显式登记 doctest 状态 | `src-tauri/crates/dp-app/src/tray_menu.rs:41`、CI 配置 | — | **P2** |

### 实现顺序（依赖图）

```
T01 ──> T02 ──┬──> T06
              ├──> T07
              └──> T09 (还需 T03)
T03 ──────────┘
T04   (独立)
T05   (独立)
T08   (独立)
T10   (独立)

关键路径: T01 → T02 → （T06/T07/T09）   ← 决定"宠物可见"
并行快赢: T05 / T08 / T10 （无依赖，可立即做）
```

**最小可交付（MVP）** = **T01 + T02 + T03**：宠物可见 + 启动不崩。

---

## 问题 5 —— 验收标准（可验证定义）

### A. 视觉呈现（对接用户投诉的核心症状）

| ID | 验收项 | 验证方法 | 判定 |
|----|--------|---------|------|
| V1 | 宠物窗口内出现**可辨识的宠物形象**，非单色方块 | 截图人工判定 + 程序探针：解出的帧图集**颜色数 > 20** 且**最大单色占比 < 30%** | 通过 |
| V2 | 形象**有透明背景**（非实心矩形） | 探针：帧内 `alpha==0` 像素占比 > 40%（当前占位物为 93%，真实角色应 50~80%） | 通过 |
| V3 | 形象**随时间动起来**（非静止方块） | 相邻帧**像素差异率 > 1%**（当前占位物虽有平移但内容单调） | 通过 |
| V4 | 形象**形状合理**（能看出头/身/尾） | 人工看图确认；辅以"非凸性"检查（轮廓非单一矩形） | 通过 |
| V5 | 无"白底/黑底"泄漏 | 窗口截图四角像素与桌面背景一致（透明有效） | 通过 |

### B. 运行稳定性

| ID | 验收项 | 验证方法 | 判定 |
|----|--------|---------|------|
| S1 | 连续运行 **≥60s 无渲染看门狗自愈** | `dev-err.log`/`pet-err.log` 中 **不出现**`渲染看门狗` / `执行自愈` | 通过 |
| S2 | 连续运行 **≥5min 无进程重启** | 日志**不出现** `连续 3 次自愈失败：重启进程`；进程 PID 不变 | 通过 |
| S3 | 启动期无致命错误 | DevTools console **无 `ReferenceError`/未捕获 rejection**；`[pet] 引导失败` 不出现 | 通过 |
| S4 | 帧回执链路健康 | `FrameWatchdog` 的 `acked_delta > 0`（日志可加一行周期打印，或从 `pet://perf` 的 fps 间接确认） | 通过 |

### C. 功能可用

| ID | 验收项 | 验证方法 | 判定 |
|----|--------|---------|------|
| F1 | 托盘菜单可打开设置窗口 | 手点托盘 → 设置窗出现且 9 个 Tab 可切换 | 通过 |
| F2 | **右键宠物可弹出菜单**（T08 修复后） | 手点右键 → 九宫格菜单完整可见可点 | 通过 |
| F3 | **提醒功能能弹动作**（T02 补 24 动作后） | 日志**不再出现** `提醒动作 ACT-P-03 未启用`；提醒到点时动作实际播放 | 通过 |
| F4 | 无"未启用/待资源"降级告警 | 全日志 `grep "未启用\|批次 C 资源未交付"` → **0 命中** | 通过 |
| F5 | 悬停/抚摸有反馈 | 手点悬停 → 出现交互动作；日志出现 `interaction 意图 Hover` | 通过 |
| F6 | 设置改动热生效 | 改透明度/缩放 → 宠物窗口即时变化（无需重启） | 通过 |

### D. 质量门禁

| ID | 验收项 | 命令 | 判定 |
|----|--------|------|------|
| Q1 | clippy 零告警 | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Q2 | Rust 测试 | `cargo test --workspace --lib --bins --tests` | 0 failed |
| Q3 | 前端测试 | `pnpm exec vitest run` | 0 failed |
| Q4 | 类型 | `pnpm exec tsc --noEmit` | 0 error |
| Q5 | 资源一致性 | 53 个 `actionId` 在 `actions.json` 与 `atlas.json` **双向一一对应**（脚本可校验） | 通过 |

> Q5 建议固化为一个**校验脚本**（`scripts/check-atlas-coverage.mjs`，新）：断言 `actions.json` 的 actionId 集合 == `atlas.json` 的 actionId 集合。这正是本次"53 vs 29"能长期潜伏的原因。

---

## 附：我对"测试体系盲区"的架构裁定

主理人的判断我完全认同，并补一条**可执行的固化建议**：

> 现有 287（前端）+ 1029（Rust）测试全绿却漏掉本次问题，根因是**所有渲染测试注入 Fake**（Fake stage / Fake adapter / Fake cache）——**从不校验真实 PNG 的像素语义**。
> 因此 **T09（装配层烟测）+ Q5（atlas 覆盖校验）是本次必须新增的"防复发"资产**，其价值高于修复本身。建议将 Q5 纳入 CI 门禁。

---

## 附：本次产生的只读分析脚本（`scripts_tmp/`，供复核）

| 文件 | 用途 |
|------|------|
| `_arch_atlas_probe.py` | 独立 PNG 手解：尺寸/通道/颜色数/主色/不透明占比 |
| `_arch_atlas_geom.py` | 逐帧色块几何（bbox/尺寸/逐帧位移），用于修正"8 帧相同"的说法 |
| `_diag_action_diff.txt` | `actions.json`(53) vs `atlas.json`(29) 差异清单 |
| `_diag_atlas_meta.txt` | 29 条图集元数据（含 `frameW`/`columns`） |
