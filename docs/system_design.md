# DesktopPet 存量项目 · 根因诊断报告

- 角色：架构师 高见远（software-architect）
- 范围：`F:\DesktopPet-Design\repo`（Tauri 2 + Rust workspace + TS 渲染层）**只读诊断，未修改任何文件**
- 用户现象：① 启动后屏幕上是「一个色块」；② 怀疑还有其他问题，要求检查测试
- 方法：逐段核实代码链路 + 实跑前端/Rust 测试套件（非推测）

---

## 0. 结论速览

| 编号 | 严重度 | 一句话结论 |
|------|--------|-----------|
| R1 | **P0** | 「色块」= **美术资源从未交付**。29 个图集全是程序生成的纯色方块；骨架 `.atlas`/`xinyuehu` 全仓库 0 命中。**补齐资源即可修复，无需改代码。** |
| R2 | **P1** | 帧回执链路在 `main.ts:160` **存在确定性 TDZ 致命错误**（`menu` 在初始化前被引用），启动即崩，**整条渲染/回执链路不执行**。但**默认配置下不可达**（SkeletonRenderer 不会同步调 `onCommand`）——是**定时炸弹**，不是当前色块的成因。 |
| R3 | **P1** | Rust 看门狗「连续 3 次自愈失败 → `app.restart()`」，而自愈动作 `WebView.reload()` **不重置 `FrameWatchdog` 计数、也不重置 `Instant start`**，构成**进程重启循环**（用户可见闪退）。 |
| R4 | **P2** | 前端「DOM 交互/菜单」实装正确，但 `DomMenuView` 与 canvas 同尺寸（256×256）时右键菜单**几何上不可见**（`clampMenuPlacement` 钳进 256² 视口）。 |
| R5 | **P2** | 纯色图集无 alpha 透明（生成器把 RGB 补成 `alpha=255`）→ 宠物区域是**不透明方块**，透明窗口语义失效。 |
| R6 | **P2** | `cargo test --workspace`（含 doctest）**非全绿**：`dp-app` doctest 报 `extern location for tauri does not exist` + 编译器 ICE。Stage10 回归报告声称「0 failed」不涵盖 doctest。 |

**「只补美术资源、不动代码，屏幕上能否正常显示宠物？」→ 能。** 见 §2 论证。

---

## 1. 证据核查与问题表

### R1 · 美术资源从未交付（P0，已证实）

| 证据 | 内容 |
|------|------|
| `resources/atlas/` | 29 个 `*_act.png` + `atlas.json`。用户/主理人实看图集 = 纯色方块（`ACT-T-01` 黄块×8、`ACT-E-01` 红块×8） |
| `scripts/gen-atlas.mjs:390-391` | `frameW: FRAME_SIZE, frameH: FRAME_SIZE` —— 图集由该脚本**程序生成**（从 `assets/sprites` 扫描序列帧打包） |
| `assets/` | 仅 `audio/`（16 个 .ogg），**无 `sprites/` 目录**（`find -type d -name sprites` → 0） |
| `find -name "*.atlas"` | **0 个** |
| `find -name "xinyuehu*"` | **0 个** |
| `resources/` | 仅 `atlas/` `config/` `schema/`，**无 `skeleton/` 目录** |
| `resources/config/character.json:22-33` | `prefer:"skeleton"`, `engine:"spine"`, `skeletonPath:"assets/sprites/xinyuehu/default/xinyuehu.json"`, `fallbackChain:["skeleton","frame"]`, `probeTimeoutMs:3000` |

**根因链（已证实）**：
`character.json` 声明首选骨架路径 → 该路径资产 0 命中 → 前端 `spineAdapter.ts:16-24` 是**明确的占位实现**（`isLoaded()` 恒 `false`、`load()` 直接 `throw`）→ `BackendSwitcher`（`BackendSwitcher.ts:56-57`）启动即走 `fallback`，3s 后 `maybeAdvance()` 命中 `timeout` 分支（`:97-101`）**定格帧回退** → 绘制 `resources/atlas` 里的纯色方块图集 → **用户看到的就是「一个色块」**。

**关键判定：色块 100% 由资源缺失导致，与渲染代码无关。** `FrameRenderer` / `WebGLStage` / `AtlasCache` 全链路工作正常（能取到图集、能解码、能上传纹理、能绘制——只是画的内容本身是色块）。

---

### R2 · `main.ts` 启动期 TDZ 致命错误（P1，已证实逻辑，待验证可达性）

**代码证据 `src/main.ts:159-162` 与 `:178`：**

```ts
// 159-162：menuView 构造时闭包捕获 menu
const menuView = new DomMenuView(overlayRoot, {
  onCommand: (id) => menu.choose(id),   // ← 引用 menu
  onClose: () => menu.close(),          // ← 引用 menu
});
...
// 178：menu 在 menuView **之后** 才用 const 声明
const menu = new MenuLayer(menuView, { ... });
```

`const menu` 位于 TDZ（暂时性死区）。`DomMenuView` 构造器把 `handlers.onCommand` **原样存字段**（`DomLayers.ts:281`），构造期**不调用**，仅在按钮 click 时求值（`DomLayers.ts:318`）。
`MenuLayer` 构造器（`MenuLayer.ts:44-49`）只存 `view` 与 `opts`，**不回调**。
`DomMenuView` 构造器（`DomLayers.ts:277-297`）不发 DOM 事件。

→ **当前默认装配下该 TDZ 不会被触发**（已逐类核实无同步回调点）。

**但这是一个真实的定时炸弹**，两个可达路径：
1. `SkeletonRenderer.ts:50-58` 的 `void this.adapter.load()` —— 若 `load()` 在 await 前**同步** resolve/reject（测试替身、或未来正式 Spine 适配器同步失败），`.catch` 在 `menuView` 构造**之前**执行 → `menu` 引用 → `ReferenceError: Cannot access 'menu' before initialization`。
2. 任何在装配期同步派发 `pet://menu` 的改动（如 Rust 侧启动期补发右键命中）→ 同样立即崩。

**性质**：与 R1 **无关**（当前色块不是它造成的）；但它意味着「渲染链路一旦崩溃即整段不执行」，是 R3 看门狗重启循环的**潜在触发源**。修复成本 2 行（把 `const menu` 上移到 `menuView` 之前，或用 `let menu` + 前置声明）。

---

### R3 · 看门狗自愈 = 进程重启循环（P1，已证实）

**代码证据 `supervisor.rs`：**

```rust
// :240-244  每 5 tick（≈5s）采样
perf_tick = perf_tick.wrapping_add(1);
if perf_tick >= PERF_EVERY_TICKS { perf_tick = 0; perf_and_watchdog_tick(...); }

// :402-433  watchdog_step：sent_delta>0 且 acked_delta==0 → miss+1；miss>=3 → Heal；heal_fail>=3 → Restart
// :368-378  Restart 分支
WatchdogStep::Restart => {
    eprintln!("[dp-app] supervisor 连续 3 次自愈失败：重启进程 + 托盘提示");
    show_tray_balloon(app, "桌面宠物渲染异常", "连续多次自动恢复失败，正在重启桌面宠物…");
    app.restart();                                    // ← 进程级重启
}

// :451-474  self_heal_renderer：只做 ①FlushSave ②webview.reload()
//            —— **不重置 FrameWatchdog 计数、不重置 supervisor 的 start(Instant)**
```

**实际后果（用户可见）**：
- 判定条件：任一 5s 窗口内 `sent_delta > 0 && acked_delta == 0`，连续 3 窗（≈15s）→ 自愈。
- 自愈动作：`WebView.reload()` 重建渲染器。**若前端因 R2（或任何启动错误）持续崩在装配期**，reload 后依然不产回执 → 再 15s 又自愈 → 再 15s → 第 3 次 → `app.restart()`。
- **`app.restart()` 后计数归零但缺陷依旧** → 进入「启动 45s → 重启」的**无限循环**，用户看到**周期性闪退/窗口消失重建**。
- **日志实证**：`dev-err.log:193-206` 完整记录了这个循环（自愈 #1 → #2 → #3 → 重启 → 新一轮启动 → 再次装配），且 `:203` 伴随 WebView 类注销错误 `Failed to unregister class Chrome_WidgetWin_0. Error = 1412`（进程被强杀的证据）。
- **对照**：`pet-err.log` 无看门狗报错且有正常交互（Hover/EarTwitch/提醒/降帧）→ 该问题**间歇性**，只在「前端装配失败」的那些启动中出现。这**恰好佐证 R2 的间歇特性**。

> ⚠️ 设计缺陷（非实现缺陷）：`self_heal_renderer` 缺少「自愈前先判断前端是否真的活着」与「reload 后重置观察窗」两个环节，导致**自愈对「启动期崩溃」这一类故障无效，且会放大为重启循环**。

---

### R4 · 右键菜单几何不可见（P2，已证实）

- `main.ts:121-122`：`canvas` 被设为 `256×256` CSS px（`LOGICAL_SIZE 128 × LOGICAL_SCALE 2`）。
- `tauri.conf.json:19-20`：pet 窗口 `width:256, height:256`。
- `index.html:18` + `pet.css:12-14`：`#pet-overlay-root { position:fixed; inset:0 }` → 覆盖层尺寸 = **256×256**。
- `MenuLayer.flush()`（`MenuLayer.ts:95-97`）用 `view.containerSize()` 做 `clampMenuPlacement`；`DomMenuView.containerSize()`（`DomLayers.ts:333-335`）返回 `rootEl.clientWidth/Height` = 256×256。

→ 菜单面板被钳制在 256² 视口内，与 2×2 字符网格的九宫格菜单**几何上必然溢出/被裁**。功能链路（`pet://menu` → `menu.open` → flush → show）是正确的，只是**没有可用空间**。同理，气泡（`--bubble-max-w:256px`）在 256² 窗内也无处安放。

> 注：`DomMenuView.containerSize()` 用的是 `this.rootEl.clientWidth`（即 `.pet-menu-root` 自身）而非传入的 `root`，语义上应为承载容器尺寸——即便 `#pet-overlay-root` 变大也仍会返回菜单自身尺寸。这是一处**次要实现瑕疵**。

---

### R5 · 图集无 alpha 透明（P2，已证实）

`scripts/gen-atlas.mjs:202-206`：
```js
// RGB → RGBA 补全 alpha=255。
rgba[i * 4 + 3] = 255;
```
纯色方块的 alpha 全 255 → 帧绘制后**整个 256×256 区域不透明**。与 `index.html:10-14`「页面背景必须保持 transparent，仅绘制角色像素，否则透明区会出现白底」的契约冲突 → 用户看到的是**实心色块**而非「宠物形状 + 透明背景」。补齐真实像素美术（含透明通道）后自动解决。

---

### R6 · 测试套件真实结果（P2，已实测）

**前端 `vitest`（实跑，全绿）**：
```
Test Files  22 passed (22)
     Tests  287 passed (287)   Duration 2.12s
```
→ 与 `tests/manual/Stage10-回归报告.md:34` 声称的 287/22 一致。**前端自动化可信。**

**Rust `cargo test --workspace`（实跑，非全绿）**：
- 各 crate 单测/集成测试通过（`dp-platform` 113 passed、`qa_s1m2_extra` 16、`qa_s1m3m4_extra` 14 passed/2 ignored、`qa_s2m6m7_probes` 4、`qa_s3m1_*` 2 等，均 0 failed）；
- **`Doc-tests dp_app` 失败**：
  ```
  error: extern location for tauri does not exist:
      ...\target\debug\deps\libtauri-6d0c31d8da3338cc.rlib
    --> crates\dp-app\src\tray_menu.rs:41:5
   41 | use tauri::image::Image;
  error: doctest failed, to rerun pass `-p dp-app --doc`
  ```
- 重跑时另遇 **rustc ICE**：`error: the compiler unexpectedly panicked. This is a bug`（rustc 1.98.1, windows-msvc，编译 `dp-app (lib)` 时，`--crate-type lib`）。
  → 判定：**环境/依赖产物层面的构建不稳定**（`tauri` rlib 缺失 + incremental 编译 ICE），非业务逻辑缺陷。但**意味着 `cargo test --workspace` 不能作为发布门禁**，Stage10 报告的「0 failed」措辞掩盖了这一项。

**测试覆盖的盲区（结构性，值得登记）**：
- `DomLayers.ts`（唯一 `document` 触点）、`main.ts`（装配点）**明确不进单测**（`DomLayers.ts:6-7` 自述）。
- → **R2（TDZ）与 R4（菜单几何）这类「装配期错误 + 真实 DOM 尺寸」缺陷，现有 287 个测试全部无法发现**。这是本次「色块 + 闪退」能在「自动化全绿」状态下流出的根本原因。

---

## 2. 核心问题回答

### Q1 帧回执链路逐段核实

| 段 | 位置 | 状态 |
|----|------|------|
| ① Rust 发 `pet://frame` | `bridge.rs:596` `app.emit(FRAME_EVENT, &cmd)`；`note_sent()` 在 `:587-589` | ✅ 正常（日志 `sent_delta=30`） |
| ② sent 计数与 emit 顺序 | `:587` 先 `note_sent()` 再 `:596` emit | ⚠️ 先计数后发射：若 emit 失败，sent 已加而前端收不到 → **只看门狗误报**（次要） |
| ③ 前端订阅 | `main.ts:208` `await listenEvent(PET_EVENT.FRAME, ...)` | ✅ 在 `bootstrapPetWindow` 尾部，**若前面 throw 则整段不执行** |
| ④ 解析 | `main.ts:209` `parseRenderFrameCmd` | ✅ |
| ⑤ 绘制 | `main.ts:214` → `renderer.draw(cmd)` → `BackendSwitcher.draw` → `FrameRenderer.consume` | ✅ |
| ⑥ 回执触发 | `main.ts:132-135` `new FrameRenderer(..., () => { host.render(); receipt.onFrameRendered(); })` | ✅ |
| ⑦ 回执节流 | `main.ts:69-83`，500ms 节流（注释写「~1s」与实际 500ms 不一致，**文档瑕疵**） | ✅ |
| ⑧ invoke | `main.ts:78` `invokeCommand('frame_receipt')` | ✅ 命令已在 `lib.rs:94` 注册 |
| ⑨ Rust 接收 | `bridge.rs:281-289` `frame_receipt` → `note_acked()` | ✅ |
| ⑩ 看门狗采样 | `supervisor.rs:309-316` → `watchdog_step` | ✅ |

**链路本身无断点。** `acked_delta=0` 的唯一解释是 **③之前整段未执行**，即 `bootstrapPetWindow()` 在 ①-③ 之间 throw。已排查的可疑 throw 点：
- `locateCanvas()`（`main.ts:100-106`）——`index.html:16` 有 `#pet-canvas` ✅ 不会 throw
- `locateOverlayRoot()`（`:109-115`）——`index.html:18` 有 `#pet-overlay-root` ✅
- **`menu` TDZ（`:160`）——唯一的确定性致命点**，但需满足 §R2 的可达条件

> ⚠️ 待验证（诚实登记）：`dev-err.log` 是**后端日志**，不含 WebView console。要 100% 钉死「哪一行 throw」，需在 `tauri dev` 中打开 DevTools 看 console 首行错误。我的判定是「**R2 是最可能的候选，但未从日志直接取证**」。

### Q2 色块与帧回执的因果关系

**色块与帧回执链路「无关」。** 证据：

- `dev-err.log:193-202`：`sent_delta=30, acked_delta=0`。若前端**完全没跑**，`FrameRenderer` 不会执行，`atlas_png` 也不会被调用——但后台 `hit_latest 掩码库就绪：actions=29 frames=210`（`:189`）说明后端图集读到了。
- 关键反证：**`pet-err.log`（另一次运行）无看门狗报错，且日志显示前端交互正常**（Hover/EarTwitch 意图、提醒到点、降帧切档）——说明前端**能**跑起来并消费事件；但即便如此，用户看到的**仍然是色块**。
- 逻辑闭环：`BackendSwitcher` 启动即 `active = fallback`（`BackendSwitcher.ts:56-57`），第一帧就走 `FrameRenderer`；`FrameRenderer.consume` 成功取图集并 `drawSubRect` 纯色方块 → **不管回执通不通，画出来的都是色块**。

**结论：只补齐美术资源（及正确透明通道），不改任何代码，屏幕上即可正常显示宠物。** 三步闭环：
1. 交付 `assets/sprites/xinyuehu/default/atlas/*.png`（真实像素帧，含 alpha）；
2. 重跑 `node scripts/gen-atlas.mjs` 重新打包 29 个图集 + `atlas.json`；
3. 若要启用骨架主路径（非必须）：补 `xinyuehu.json` + `xinyuehu.atlas` + 正式 `spineAdapter`（当前 `spineAdapter.ts` 是占位，`load()` 直接 throw）。

### Q3 Rust 看门狗逻辑

见 §R3。判定条件：`watchdog_step`（`supervisor.rs:402-433`）—— `sent_delta==0` → 清零观察（正确，避免误判暂停）；`acked_delta>0` → 健康并清 `heal_fail`；否则 `miss+1`，`miss>=3` → `Heal`（`heal_fail+1`），`heal_fail>=3` → `Restart`。
自愈动作（`:451-474`）：仅 `FlushSave` + `webview.reload()`；**不重置计数与时间窗**。
后果：**对「启动期崩溃」类故障无效且会放大为 `app.restart()` 无限重启循环**（用户可见闪退）。

### Q4 其他影响「正常使用」的断点

| 项 | 核实结果 |
|----|---------|
| `resources/config/*.json` vs schema | ✅ 12 个配置文件 + `resources/schema/` 存在；`RendererCfg`（`model.rs:632-670`）与 `character.json:22-33` **字段完全对齐**（含 `probeTimeoutMs`/`frameFallbackPath`/`atlasMemBudgetMb`）。**但全仓库无任何 Rust 代码消费 `RendererCfg` 的路径字段**（`grep renderer.` → 0 命中）——即 `skeletonPath`/`atlasPath` **只是配置声明，无实际加载器**。骨架路径「失败」不是「加载报错」，而是**根本没有加载代码** + 前端占位适配器。 |
| `src-tauri/dist` vs `devUrl` | ✅ 两者并存且正确：`tauri.conf.json:9-11` `devUrl=http://localhost:5173` + `frontendDist=dist`；`vite.config.ts` `outDir:'src-tauri/dist'` 与之对齐。`dist/` 现为 2026-09-19 12:37 产物（与 `dev-err.log` 的 dev 运行时间吻合，属正常残留）。**dev 模式走 vite devServer，`dist` 不参与**——不是缺陷。 |
| `index.html` CSP | ✅ `index.html:7` 内联 CSP 与 `tauri.conf.json:51-52` 的 `csp`/`devCsp` **一致**（`connect-src` 含 `ipc: http://ipc.localhost`，dev 额外放开 `localhost:5173`/`ws:`）。`script-src 'self'` 不阻断 vite dev 的 module 加载（devCsp 已含 `unsafe-eval`）。**无问题**。 |
| 透明窗口 | ✅ `transparent:true, decorations:false, shadow:false, skipTaskbar:true`（`tauri.conf.json:26-29`）；`lib.rs:207-210` 调 `ensure_styles()` + `set_topmost(Always)`。**配置正确**——但受 R5（alpha=255）影响，透明语义在**内容层**失效。 |
| `capabilities/default.json` | ✅ 最小权限集 `["core:event:default"]`，窗口 `["pet","settings"]`，与 `main.ts` 只需 `listen`+`invoke` 一致。`frame_receipt`/`atlas_png`/`menu_command` 是**自定义命令**，不受 capability 约束。**无问题**。 |
| 键盘/鼠标钩子 | ✅ 双 gate 装配正确（`lib.rs:291-312`），`set_click_through_observer` 链式下发。 |

---

## 3. 修复优先级建议

| 优先级 | 动作 | 影响 | 工作量 |
|--------|------|------|--------|
| **P0-1** | **交付真实美术资源**：`assets/sprites/xinyuehu/default/atlas/` 像素帧（含 alpha）→ 重跑 `node scripts/gen-atlas.mjs` | 直接消除「色块」，宠物可见 | 美术交付 |
| **P0-2** | 资源目录纳入版本管理（`assets/sprites/` 当前缺失，`resources/atlas` 是占位产物却已入库） | 防止再次「占位资源冒充交付」 | 小 |
| **P1-1** | **修 `main.ts` TDZ**：`const menu` 上移到 `menuView` 之前 | 消除启动期致命错误定时炸弹，直接降低 R3 触发概率 | 2 行 |
| **P1-2** | **修看门狗自愈**：`self_heal_renderer` 里 ① `reload()` 前先查前端存活（如探测命令）② `reload()` 后 **重置 `FrameWatchdog` 计数并重锚 `start`**；③ `Restart` 前加「本次会话重启次数上限」熔断 | 消除进程重启循环/闪退 | 中 |
| **P1-3** | `main.ts` `bootstrapPetWindow()` 整体加 `try/catch` + `console.error` 上报；`void bootstrapPetWindow()` → `void bootstrapPetWindow().catch(...)`（当前**未捕获的 Promise rejection 会被静默吞掉**，这正是「前端崩了但日志无痕」的原因） | 让装配期故障可观测 | 小 |
| **P2-1** | `DomMenuView.containerSize()` 改用承载容器 `root` 尺寸；确认菜单/气泡在 256² 窗内的可用几何（必要时放大 pet 窗口或让菜单溢出到窗口外） | 右键菜单/气泡可用 | 小 |
| **P2-2** | 新增装配层烟测：用 jsdom/Playwright 跑一遍 `bootstrapPetWindow()`，断言「无 throw + 收到 frame 后 `frame_receipt` 被 invoke」 | 补上 287 个测试的结构性盲区 | 中 |
| **P2-3** | 修 `dp-app` doctest（`tray_menu.rs:41` 的 `use tauri::...` 在 doctest 上下文 rlib 缺失）；CI 用 `--lib --bins --tests` 并**显式登记** doctest 状态 | 门禁诚实 | 小 |

### 最小修复路径（让宠物正常显示 + 正常运行）

**第 1 步（必做，可独立见效）**：交付 `assets/sprites/xinyuehu/default/atlas/*.png` 真实像素帧（含 alpha）→ `node scripts/gen-atlas.mjs` 重打包。
→ **此时启动即可看到宠物**（帧回退路径已证明是通的）。

**第 2 步（必做，消除闪退）**：`main.ts` 把 `const menu` 上移 2 行 + `bootstrapPetWindow` 加 `try/catch` + `catch` 上报。
→ 消除启动期致命错误，看门狗不再被误触发。

**第 3 步（建议，止住复发）**：`supervisor.rs::self_heal_renderer` 增加「reload 后重置计数/重锚窗口」+「重启次数熔断」。
→ 即使未来出现新的前端故障，也只表现为「一次自愈」而非「进程重启循环」。

**第 4 步（可选）**：接入正式 Spine 4.2 运行时（替换 `spineAdapter.ts` 占位）——**非必需**，帧路径已可完整呈现宠物。

---

## 4. 待验证事项（诚实登记）

1. **R2 的实际触发行**——需在 `tauri dev` DevTools console 取首行错误。我已确认「TDZ 逻辑真实存在、默认装配下不可达」，但**未从日志直接取证**它是否就是 `dev-err.log` 那次运行的成因。
2. `dev-err.log` 与 `pet-err.log` 的差异原因（为何一次崩一次正常）——推测与 R2 的 `load()` 微时序竞态有关，需多次启动复现。
3. `cargo test --workspace` 的 doctest 失败与 ICE 是「稳定复现的环境问题」还是「偶发」——本次 3 次运行中 2 次 ICE、1 次成功，倾向环境/增量编译层面。
4. `resources/atlas` 的 29 个色块图集**已入库**（`.gitignore:13` 忽略 `resources/**/*` 但文件实际存在）——需确认它们是否为「误提交的占位产物」，以免与真实资源混淆。
