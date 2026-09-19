/**
 * 简体中文文案包（`01 FR-7-7`；**默认语言**，`03 S5-M5`）。
 *
 * 约定（C2 / C7）：
 * - 键名一律 `域.分组.项` 三段小驼峰（如 `appearance.scale.label`），**禁止**中文键名；
 * - 角色名**绝不写入文案**：一律用 `{name}` 占位，由 `renderPlaceholders` 在渲染期替换
 *   （`01 §6.16.4`「命名对 UI 的影响逐处清单」）；
 * - 数值类文案不写死数值（开关/滑块的值由配置驱动），文案只提供标签与枚举名；
 * - 本文件是文案真源，`en-US.ts` 的键集合必须与本文案**完全一致**（由 parity 单测锁定）。
 */
import type { Messages } from './index';

/** 简体中文文案表（flat dotted keys）。 */
export const ZH_CN: Messages = {
  // --- 应用外壳 -----------------------------------------------------------
  'app.title': '{name} 设置',
  'app.tab.appearance': '外观',
  'app.tab.behavior': '行为',
  'app.tab.interaction': '互动',
  'app.tab.needs': '属性',
  'app.tab.activity': '活动',
  'app.tab.shop': '商城',
  'app.tab.data': '数据',
  'app.tab.achievement': '成就',
  'app.tab.album': '相册',
  'app.footer.save': '保存',
  'app.footer.reset': '重置全部数据',
  'app.dirty.hint': '有未保存的改动',
  'app.saved.hint': '已保存',
  'app.readonly.hint': '设置服务不可用（只读预览）',

  // --- 通用 ---------------------------------------------------------------
  'common.ok': '确定',
  'common.cancel': '取消',
  'common.confirm': '确认',
  'common.on': '开',
  'common.off': '关',
  'common.loading': '加载中…',
  'common.unavailable': '不可用',
  'common.percent': '{value}%',

  // --- 外观（FR-7-1 / FR-7-2 / FR-7-6 / FR-7-7） --------------------------
  'appearance.name.label': '名字',
  'appearance.name.placeholder': '输入她的名字',
  'appearance.name.hint': '台词里的 {name} 会随之替换',
  'appearance.scale.label': '大小',
  'appearance.opacity.label': '透明度',
  'appearance.language.label': '语言',
  'appearance.language.zh-CN': '简体中文',
  'appearance.language.en-US': 'English',

  // --- 声音（FR-7-3） -----------------------------------------------------
  'audio.volume.label': '音量',
  'audio.muted.label': '静音',

  // --- 行为（FR-7-4 / FR-1-2 / FR-1-9） -----------------------------------
  'behavior.autoRoam.label': '允许自动走动',
  'behavior.roamPace.label': '节奏',
  'behavior.roamPace.slow': '慢',
  'behavior.roamPace.normal': '正常',
  'behavior.roamPace.fast': '快',
  'behavior.topmost.label': '置顶策略',
  'behavior.topmost.always': '始终置顶',
  'behavior.topmost.belowFullscreen': '全屏之下',
  'behavior.topmost.never': '从不置顶',
  'behavior.clickThrough.label': '鼠标穿透',
  'behavior.clickThrough.hint': '开启后托盘「摸摸」仍可用',
  'behavior.autostart.label': '开机自启',
  'behavior.dnd.label': '勿扰模式',
  'behavior.dnd.hint': '暂停气泡与主动漫游，仅保留待机动画',

  // --- 互动（FR-7-9 / FR-7-10 / FR-4） ------------------------------------
  'interaction.sensitivity.label': '她的黏人程度',
  'interaction.sensitivity.relaxed': '慢热',
  'interaction.sensitivity.normal': '刚刚好',
  'interaction.sensitivity.clingy': '超黏人',
  'interaction.catchphrase.enabled': '口头禅「心心」',
  'interaction.catchphrase.frequency.label': '频率',
  'interaction.catchphrase.frequency.off': '关闭',
  'interaction.catchphrase.frequency.low': '低',
  'interaction.catchphrase.frequency.standard': '标准',
  'interaction.catchphrase.frequency.high': '高',
  'interaction.clickFeedback.label': '点击微反馈',
  'interaction.easyCoax.label': '轻松模式',
  'interaction.easyCoax.hint': '抚摸满 2 秒即可哄好（默认 5 秒）',
  'interaction.privacy.label': '活跃感知',
  'interaction.privacy.hint': '关闭后退化为纯时间模型',

  // --- 提醒与勿扰（FR-10-2 / FR-10-4，读 schedule.json） -------------------
  'reminder.section': '提醒',
  'reminder.sedentary.label': '久坐提醒',
  'reminder.sedentary.interval': '间隔',
  'reminder.water.label': '喝水提醒',
  'reminder.water.interval': '间隔',
  'reminder.ackResets.label': '点击「知道了」重新计时',
  'reminder.minutes': '{value} 分钟',

  // --- 属性（FR-12.6，骨架：数值由 S7/S8 内核推送） -----------------------
  'needs.satiety': '饱食度',
  'needs.cleanliness': '清洁度',
  'needs.energy': '体力',
  'needs.mood': '心情',
  'needs.affinity': '亲密度',
  'needs.boredom': '无聊度',
  'needs.affinity.progress': 'Lv{level}·{exp}/{next}',
  'needs.affinity.max': 'Lv{level}·满级',
  'needs.placeholder': '属性面板骨架已就绪，实时数值随 S7 属性系统接入。',

  // --- 活动（S10-M1；`01 §8.4`；FR-13） -----------------------------------
  'activity.idle.title': '暂无外出',
  'activity.idle.hint': '选择下方活动派遣她外出：打工 / 学习 / 旅游。',
  'activity.kind.work': '打工',
  'activity.kind.study': '学习',
  'activity.kind.travel': '旅游',
  'activity.remaining': '剩余 {ms}',
  'activity.recall': '提前召回',

  // --- 商城 / 背包（S10-M1；`01 §8.5`；FR-14） ----------------------------
  'shop.balance': '心币 {coin}',
  'shop.inventory.section': '背包',

  // --- 相册 / 桌面装饰（S10-M1；`01 §8.6` / FR-13-6） --------------------
  'album.photos.title': '我的相册',
  'album.photos.empty': '还没有照片，去旅游寄张明信片吧～',
  'album.frame.hint': '在商城购买「{frame}」后，可把照片摆成桌面相框。',
  'decor.title': '桌面装饰',
  'decor.pick.title': '选择摆件',
  'decor.pick.empty': '暂无可摆放的摆件（在商城「摆件」分类购买）。',
  'decor.slots.title': '桌面 5 槽',
  'decor.slot.empty': '槽 {n}（空）',

  // --- 情绪原因卡（S10-M2；`01 §6.12` P 因子可解释性） --------------------
  'reason.title': '她为什么委屈？',
  'reason.pressure': '冷落压力 {p}/{cap}',
  'reason.none': '现在情绪不错（{state}），继续保持～',
  'reason.dir.faster': '加速冷落 ×{w}',
  'reason.dir.slower': '缓解 ×{w}',

  // --- 数据（FR-8-2 / FR-8-4，含存档健康提示） ----------------------------
  'data.save.section': '存档',
  'data.save.path': '存档路径',
  'data.save.state.fresh': '全新存档',
  'data.save.state.loaded': '正常',
  'data.save.state.recovered': '已从备份恢复',
  'data.save.state.isolated': '已隔离损坏档并重建默认档',
  'data.save.state.future': '存档版本过高，已隔离',
  'data.save.state.migrationPending': '待迁移（v1 旧档）',
  'data.save.writable': '可写',
  'data.save.readonly': '只读（不写盘）',
  'data.save.backups': '可用备份',
  'data.save.import': '导入存档',
  'data.save.import.hint': '从 save.json.bak / save.json.v1bak / save.corrupt.* 恢复',
  'data.save.import.done': '已导入并重载',
  'data.save.import.none': '未发现可用备份',
  'data.reset.section': '重置',
  'data.reset.label': '重置全部数据',
  'data.reset.confirm.title': '确认重置全部数据？',
  'data.reset.confirm.body':
    '六维数值、心币、背包、活动、技能、相册、成就与设置将全部恢复默认，且不可撤销。',
  'data.reset.done': '已重置为默认档',
  'data.session.section': '情绪兜底',
  'data.session.reset': '重置情绪',
  'data.session.reset.hint': '强制解除 L5 离家状态，走回桌面',
  'data.session.recall': '把 {name} 找回来',

  // --- 成就（`01 §8.5`；P1，骨架占位） -----------------------------------
  'achievement.section': '成就',
  'achievement.progress': '已达成 {done}/{total}',
  'achievement.locked': '未解锁',
  'achievement.placeholder': '成就系统骨架已就绪，定义与进度随 S8 养成模块接入。',

  // --- 托盘 / 对话框（供原生层与设置页共用文案键） ------------------------
  'dialog.quit.title': '退出 {name}？',
  'dialog.quit.body': '退出前会自动保存进度。',
  'dialog.saveCorrupt.title': '存档已损坏并自动恢复',
  'dialog.saveCorrupt.body': '原存档已隔离保留，可从设置页「数据」Tab 导入恢复。',
  'tray.settings': '设置',
  'tray.about': '关于',
  'tray.quit': '退出',
};

/** 文案键联合类型（zh-CN 为真源；其余语言包必须同键集）。 */
export type MessageKey = keyof typeof ZH_CN;
