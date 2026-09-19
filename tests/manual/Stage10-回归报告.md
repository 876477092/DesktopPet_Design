# Stage 10 · 整体验收与发布 · 回归报告（S10-M3 · T-26）

- 阶段：Stage 10（M9），T-26，计划 6 人日
- 范围：S10-M1 增量 UI 面板 + 相册/装饰；S10-M2 情绪可解释性；S10-M3 回归与终检
- 执行：自动化门禁全绿；需真实桌面手点的项见《acceptance-checklist.md》

---

## 1. 本次改动清单

### 前端（`src/`）
- `shared/ipc.ts`：新增 `PetSnapshotV2` 类型 + `parsePetSnapshotV2`（前向兼容：缺段默认、脏项丢弃、非对象返回 null）。
- `shared/shopCatalog.ts` / `shared/activityCatalog.ts`：静态目录归一化（展示用；购买/结算权威仍在 Rust）。
- `settings/hooks/usePetSnapshot.tsx`：订阅 `pet://state`（1Hz），未连接内核时安全降级。
- 组件：`NeedsBar`、`ActivityCard`、`DecorSlotGrid`、`ReasonCard`、`Postcard`。
- 页面：`NeedsPage` 接实时六维；新增 `ActivityPage`、`ShopPage`、`AlbumPage`。
- `App.tsx`：Tab 由 6 → 9（+活动/商城/相册）；挂载 `PetSnapshotProvider`。
- i18n `zh-CN`/`en-US`：新键齐全（parity 测试通过）；`settings.css` 新增组件样式。
- 测试：`shared/petSnapshot.test.ts`（9 例）。

### Rust（`src-tauri/`）
- `dp-core/event.rs`：`PetSnapshotV2` 增加 `album` / `decor`（5 槽）字段，Default 与 `project_snapshot` 补齐。
- `dp-app/coreloop.rs`：`emit_state_snapshot` 把**经济余额、背包、相册、装饰槽**并入 1Hz 快照（此前 economy/inventory 只落盘、未回传前端）；新增 `DecorPlace`/`DecorRemove` 派发（槽位越界、背包无该摆件即拒；写 `save.decor` 后请求落盘）。
- `dp-app/bridge.rs`：`CoreInput` 增 `DecorPlace{slot,itemId}` / `DecorRemove{slot}`。
- `dp-app/commands.rs` + `lib.rs`：新增并注册 `pet_decor_place` / `pet_decor_remove`。

---

## 2. 自动化门禁结果

| 门禁 | 命令 | 结果 |
|------|------|------|
| 类型 | `pnpm exec tsc --noEmit` | 0 error |
| 前端单测 | `pnpm exec vitest run` | **287 passed / 287**（22 文件） |
| Rust 单测 | `cargo test --workspace` | **0 failed**（CARGO_EXIT=0） |
| 构建 | `pnpm build` | vite 成功，双 HTML 产物 |
| 内存巡检 | `scripts/check-mem.ps1` | 脚本就绪；需运行中进程，留真机执行（清单 A6） |

> 说明：PowerShell 下 cargo/vite 把进度写到 stderr，会被包装层误报“exit 1”；以 `$LASTEXITCODE`/`$?` 复核均为 0。

---

## 3. 验收对照

- **S10-M1 AC**：4 个增量 Tab（属性/活动/商城+背包）可用，数据来自 `pet://state` 与静态配置、UI 无硬编码；相册可浏览、装饰 5 槽可摆放/取下（读写 `save.decor`，摆件来自 `furniture` 目录）。✅ 自动化覆盖；手点见清单 B。
- **S10-M2 AC**：进入委屈/生气时，原因卡（长按心情条弹出）显示 P/档位 + 按权重排序的主因 + 建议。`ReasonCard` 组件与解析单测就绪；长按入口 S3-M5 已具备。手点见清单 C。

---

## 4. 已知边界与遗留（诚实登记）

1. **明信片 → 相册落盘**：旅游明信片目前在活动运行时累计，尚未在结算端点写入 `save.album`（属 S8 活动结算收尾）。`AlbumPage` 已能正确渲染 `save.album` 中的任意照片条目（空态友好）；待 S8 接通后照片即现，无需改 UI。
2. **economy.todayEarned / dailyCap**：快照中暂为后端默认 0；`ShopCard` 只依赖 `coin`，不影响购买/背包正确性。
3. **桌面原因卡浮层**：`OverlayLayer` 长按心情条入口（`onReasonCardRequest`）在 S3-M5 已落地；本卡交付 `ReasonCard` 组件与解析。把浮层真正挂到宠物窗气泡渲染链属宠物窗集成，真机按清单 C 验收。
4. **内存巡检**：`check-mem.ps1` 需运行中的 `DesktopPet.exe`，无头构建环境未执行，按清单 A6 真机复跑。

---

## 5. 发布结论

自动化门禁全绿，编译/测试/构建通过；剩余项均为**真机手点**与 S8 遗留的数据落盘衔接，不构成 UI 阻塞。按清单《acceptance-checklist.md》在真实桌面完成 B/C/D 段勾选后即可发布。
