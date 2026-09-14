# desktop-pet · 工程骨架（T-01 / S1-M1）

Tauri 2 + Rust 内核 + TypeScript/WebView2 前端的桌面宠物工程骨架。
本阶段**只搭骨架与构建管线**，不包含任何业务逻辑。

- 架构单一真源：`DesktopPet-Design/02-技术架构设计.md`（相对本仓库上级目录）
- 施工图：`DesktopPet-Design/03-分模块实施计划与执行台账.md` §2 Stage 1 · S1-M1
- 门禁结论：`DesktopPet-Design/gate/STAGE-G-REPORT.md`（SG-M1 / SG-M2 双 Go）

---

## 1. 环境准备

| 工具 | 版本要求 | 说明 |
|---|---|---|
| Node.js | ≥ 20（实测 v22.22.2） | 前端与脚本 |
| pnpm | ≥ 9 | 包管理器；Corepack 不可用时用 `npm i -g pnpm` |
| Rust | stable ≥ 1.77.2 | `rustup toolchain install stable-x86_64-pc-windows-msvc` |
| MSVC 生成工具 | VS 2022 BuildTools | 链接阶段需要；脚本会自动探测 `vcvars64.bat`，也可用环境变量 `VCVARS64` 指定 |
| WebView2 运行时 | 系统已装即可 | 精简包依赖系统运行时 |

> MSVC 环境：脚本默认用 `vswhere` 自动探测；也可在执行前手动导入：
> `cmd /c "<VS安装路径>\VC\Auxiliary\Build\vcvars64.bat" && set` 后带入同一会话。
> **脚本内不硬编码任何盘符路径（C1）。**

## 2. 常用命令

```bash
pnpm install              # 安装前端依赖（生成 pnpm-lock.yaml）
pnpm dev                  # 仅起 vite dev server（localhost:5173）
pnpm build                # 仅构建前端双入口产物
pnpm typecheck            # tsc --noEmit（strict）
pnpm lint                 # eslint（flat config）

pnpm tauri:dev            # 起 Tauri：vite dev + pet/settings 双窗口
pnpm tauri:build          # 构建 DesktopPet.exe 与 NSIS 安装包

# 等价的一键脚本（会自动探测并导入 MSVC 环境）
pwsh ./scripts/dev.ps1
pwsh ./scripts/build.ps1
```

Rust 侧（需先导入 MSVC 环境）：

```bash
cd src-tauri
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo build            # 产出 target/debug/DesktopPet.exe
```

## 3. 双窗口与构建管线

| 入口 | HTML | 窗口 label | 用途 |
|---|---|---|---|
| `pet` | `index.html` → `src/main.ts` | `pet` | 宠物窗口：透明、无边框、无任务栏项 |
| `settings` | `settings.html` → `src/settings/main.tsx` | `settings` | 设置窗口：React 挂载点 |

- **vite 产物目录**：`src-tauri/dist`（见 `vite.config.ts` 的 `build.outDir`）
- **Tauri 前端目录**：`src-tauri/tauri.conf.json` 的 `build.frontendDist = "dist"`（相对 `src-tauri/`）
  两者必须保持一致，改任一方都要同步另一方。
- **窗口声明**：`pet` / `settings` 两个窗口**完全由 `tauri.conf.json` 的 `app.windows` 声明生成**，
  Rust 侧不写任何窗口操控代码（窗口行为属 S1-M2 / T-02）。
- **二进制名**：`dp-app` 的 `[[bin]] name = "DesktopPet"`，与 `tauri.conf.json` 的
  `mainBinaryName = "DesktopPet"` 一致，故 `cargo build` 产出 `DesktopPet.exe`。

窗口关键参数（宠物窗口）：

| 参数 | 值 | 出处 |
|---|---|---|
| `transparent` | `true` | `02 §2.3` / SG-M1 |
| `decorations` | `false` | S1-M1 卡片 |
| `skipTaskbar` | `true` | S1-M1 卡片 |
| `visibleOnAllWorkspaces` | `true` | S1-M1 卡片 |
| `additionalBrowserArgs` | 禁用 WebView2 遥测与后台联网（★ Tauri 2 实际字段名，非 `...Arguments`） | `02 §1.4` L-05 |

> ⚠️ 骨架阶段设置窗口同样为 `decorations: false`，且未接托盘菜单，
> 关闭方式依赖系统（Alt+F4 / 任务管理器）。真实边框、托盘入口与关闭行为由 S1-M2 / S3 补齐。

## 4. 目录结构（骨架范围）

```
repo/
├── index.html / settings.html      # 双入口页面
├── vite.config.ts / tsconfig*.json / tailwind.config.ts / postcss.config.js / eslint.config.js
├── src/
│   ├── main.ts                     # 宠物窗口引导（画布尺寸 + DPR 重建）
│   ├── settings/main.tsx           # 设置窗口 React 挂载点
│   ├── shared/{ipc,types,coords}.ts # 共享占位：IPC 封装 / 事件名常量 / 坐标换算
│   └── styles/{pet,settings}.css
├── src-tauri/
│   ├── Cargo.toml                  # workspace（members = crates/*）
│   ├── build.rs  tauri.conf.json  capabilities/default.json
│   ├── icons/                      # 应用图标（占位生成的纯色图）
│   └── crates/
│       ├── dp-app/                 # ★ Tauri 应用层（main.rs / lib.rs）
│       └── dp-core  dp-activity  dp-economy  dp-platform  dp-assets  dp-audio
├── resources/{config,schema}/       # 占位目录，配置内容属 S1-M5
├── assets/                          # 占位目录，美术资源属 S1-M5
└── scripts/{dev.ps1,build.ps1,gen-types.mjs,gen-atlas.mjs}
```

## 5. 运行期数据目录（`03 §0.1`）

| 用途 | 路径 |
|---|---|
| 存档 / 配置覆盖 | `%APPDATA%\DesktopPet\` |
| 日志 | `%LOCALAPPDATA%\DesktopPet\logs\` |
| 安装目录 | `%LOCALAPPDATA%\DesktopPet\` |
| 资源（只读） | `<install>\resources\` |

## 6. 硬约束自查清单

| # | 约束 | 骨架阶段落实情况 |
|---|---|---|
| C1 | 代码内禁止盘符绝对路径字面量 | 全部路径相对工程根或走环境变量；`scripts/*.ps1` 用 `vswhere` / `$env:VCVARS64` |
| C2 | 角色名走 `{name}` 占位符，禁止硬编码 | 骨架阶段未出现任何角色名字面量 |
| C3 | 时间读取必须经 `WallClock` 端口 | 骨架无时间逻辑；`dp-app` 仅启动运行时 |
| C4 | 禁 GPL/LGPL 依赖 | 依赖仅 Tauri 2（MIT OR Apache-2.0）与前端 MIT/Apache-2.0 包 |
| C5 | 动作 ID `ACT-*` 与任务号 `T-xx` 不混用 | 骨架无动作 ID；注释中统一用 `T-xx` |
| C7 | 配置键 camelCase + 单位后缀 | 骨架无配置键；`src/shared` 已按规范命名 |
| C9 | 零对外连接 | `capabilities/default.json` 权限全关；CSP 禁外链；无 http/updater/shell/fs 插件 |

## 7. 已知限制

1. **未做 Win10 1809 真机验证**：开发环境为 Win11，`02 §2.3` 要求的双机型矩阵待补测。
2. **`capabilities/default.json` 权限全关**：若后续 `invoke` 需求出现，须逐条按最小必需集申请并登记理由。
3. **资源脚本进度**：`scripts/gen-atlas.mjs` 已实现（序列帧 → 横向图集 + `atlas.json`，
   含 PNG 编解码与 alpha 命中统计，S1-M5 交付）；`scripts/gen-types.mjs` 仍为占位脚本
   （运行后仅打印指引并 `exit 0`）。
4. **图标为占位纯色图**：正式美术图标随资源管线替换。ICO 必须是**经典 BMP 格式**：
   PNG 内嵌 ICO 会让 `rc.exe` 死循环并写出数十 GB 的 `.res` 文件（实测踩坑）。
5. **受限环境（沙箱/构建机）需 `.npmrc` 的 `node-linker=hoisted` + `package-import-method=copy`**：
   沙箱会静默拒绝创建符号链接/硬链接，导致 `node_modules/<pkg>` 空目录、`.bin` 断链；
   普通开发机上该配置无副作用。此外 pnpm 12 下 `pnpm exec` 在本机不可用，
   请改用 `pnpm run <script>`（如 `pnpm typecheck` / `pnpm lint` / `pnpm build`）或直接调用
   `node node_modules/<pkg>/bin/...`。
6. **窗口样式位判据勘误（2026-09-12 QA 复核定版）**：真机探针中 pet 窗口
   `windowRect == clientSize`（256×256，非客户区增量 0×0）——`decorations:false`
   **已生效**（tao 通过 `WM_NCCALCSIZE` 返回 0 实现无边框，保留 `WS_CAPTION`
   标志位属正常现象）；`transparent:true` 走 DWM blur-behind + WebView2
   `DefaultBackgroundColor=(0,0,0,0)`，不产生 `WS_EX_LAYERED`；`skipTaskbar:true`
   走 `ITaskbarList::DeleteTab`。运行时 A/B 像素对照（窗口开/关同矩形抓屏逐位一致）
   证实透明区贡献 0 像素、无白底。`WS_EX_TOOLWINDOW`（Alt+Tab 隐藏）与
   `WS_EX_NOACTIVATE`（不抢焦点）是 tao 不会自动加的位，属 S1-M2
   `PlatformWindow` + 30s `ensure_styles` 职责（补写时必须 `|=` 叠加，
   保留系统管理的 `WS_EX_TOPMOST`）。详见 `gate/s1-m1/qa/qa-record.md §1`。
7. **`visibleOnAllWorkspaces` 在 Windows 上是 no-op**（tao 仅 macOS/Linux 实现），
   保留该配置是为跨平台前向兼容，勿据其得出「Windows 虚拟桌面已支持」的结论。
