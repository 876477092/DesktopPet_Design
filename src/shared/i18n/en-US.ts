/**
 * English (US) message catalog — `01 FR-7-7`（P2 多语言预留，`03 S5-M5` 骨架）。
 *
 * 口径：
 * - **键集合与 `zh-CN.ts` 完全一致**（parity 由单测锁定）：缺键会让切换语言后部分文案
 *   回退中文，属「骨架不完整」，故本骨架一次性给全；
 * - 角色名同样走 `{name}` 占位，**不得**写入任何具体名字（C2）；
 * - 本文件当前为**机器可读的骨架译文**：文案可后续由产品/翻译替换，但键名与占位符
 *   不得改动（改动即契约变更）。
 */
import type { Messages } from './index';

/** English (US) 文案表（键集合 = `zh-CN.ts`）。 */
export const EN_US: Messages = {
  // --- App shell ---------------------------------------------------------
  'app.title': '{name} Settings',
  'app.tab.appearance': 'Appearance',
  'app.tab.behavior': 'Behavior',
  'app.tab.interaction': 'Interaction',
  'app.tab.needs': 'Stats',
  'app.tab.activity': 'Activity',
  'app.tab.shop': 'Shop',
  'app.tab.data': 'Data',
  'app.tab.achievement': 'Achievements',
  'app.tab.album': 'Album',
  'app.footer.save': 'Save',
  'app.footer.reset': 'Reset all data',
  'app.dirty.hint': 'Unsaved changes',
  'app.saved.hint': 'Saved',
  'app.readonly.hint': 'Settings service unavailable (read-only preview)',

  // --- Common ------------------------------------------------------------
  'common.ok': 'OK',
  'common.cancel': 'Cancel',
  'common.confirm': 'Confirm',
  'common.on': 'On',
  'common.off': 'Off',
  'common.loading': 'Loading…',
  'common.unavailable': 'Unavailable',
  'common.percent': '{value}%',

  // --- Appearance --------------------------------------------------------
  'appearance.name.label': 'Name',
  'appearance.name.placeholder': 'Enter her name',
  'appearance.name.hint': '{name} in dialogue follows this name',
  'appearance.scale.label': 'Size',
  'appearance.opacity.label': 'Opacity',
  'appearance.language.label': 'Language',
  'appearance.language.zh-CN': '简体中文',
  'appearance.language.en-US': 'English',

  // --- Audio -------------------------------------------------------------
  'audio.volume.label': 'Volume',
  'audio.muted.label': 'Mute',

  // --- Behavior ----------------------------------------------------------
  'behavior.autoRoam.label': 'Allow roaming',
  'behavior.roamPace.label': 'Pace',
  'behavior.roamPace.slow': 'Slow',
  'behavior.roamPace.normal': 'Normal',
  'behavior.roamPace.fast': 'Fast',
  'behavior.topmost.label': 'Always on top',
  'behavior.topmost.always': 'Always',
  'behavior.topmost.belowFullscreen': 'Below fullscreen',
  'behavior.topmost.never': 'Never',
  'behavior.clickThrough.label': 'Click-through',
  'behavior.clickThrough.hint': 'The tray "pet" action still works',
  'behavior.autostart.label': 'Start with Windows',
  'behavior.dnd.label': 'Do not disturb',
  'behavior.dnd.hint': 'Pauses bubbles and roaming; idle animation only',

  // --- Interaction -------------------------------------------------------
  'interaction.sensitivity.label': 'How clingy she is',
  'interaction.sensitivity.relaxed': 'Slow to warm',
  'interaction.sensitivity.normal': 'Just right',
  'interaction.sensitivity.clingy': 'Very clingy',
  'interaction.catchphrase.enabled': 'Catchphrase',
  'interaction.catchphrase.frequency.label': 'Frequency',
  'interaction.catchphrase.frequency.off': 'Off',
  'interaction.catchphrase.frequency.low': 'Low',
  'interaction.catchphrase.frequency.standard': 'Standard',
  'interaction.catchphrase.frequency.high': 'High',
  'interaction.clickFeedback.label': 'Click feedback',
  'interaction.easyCoax.label': 'Easy mode',
  'interaction.easyCoax.hint': 'Stroking for 2 seconds is enough (5s by default)',
  'interaction.privacy.label': 'Activity sensing',
  'interaction.privacy.hint': 'When off she falls back to a pure time model',

  // --- Reminders ---------------------------------------------------------
  'reminder.section': 'Reminders',
  'reminder.sedentary.label': 'Sedentary reminder',
  'reminder.sedentary.interval': 'Interval',
  'reminder.water.label': 'Water reminder',
  'reminder.water.interval': 'Interval',
  'reminder.ackResets.label': 'Acknowledge restarts the timer',
  'reminder.minutes': '{value} min',

  // --- Stats -------------------------------------------------------------
  'needs.satiety': 'Satiety',
  'needs.cleanliness': 'Cleanliness',
  'needs.energy': 'Energy',
  'needs.mood': 'Mood',
  'needs.affinity': 'Affinity',
  'needs.boredom': 'Boredom',
  'needs.affinity.progress': 'Lv{level} · {exp}/{next}',
  'needs.affinity.max': 'Lv{level} · Max',
  'needs.placeholder':
    'The stats panel skeleton is ready; live values arrive with the S7 needs system.',

  // --- Activity (S10-M1; `01 §8.4`; FR-13) --------------------------------
  'activity.idle.title': 'She is not away right now',
  'activity.idle.hint': 'Pick an activity below to send her out: work / study / travel.',
  'activity.kind.work': 'Work',
  'activity.kind.study': 'Study',
  'activity.kind.travel': 'Travel',
  'activity.remaining': '{ms} left',
  'activity.recall': 'Recall early',

  // --- Shop / inventory (S10-M1; `01 §8.5`; FR-14) ------------------------
  'shop.balance': '{coin} coins',
  'shop.inventory.section': 'Inventory',

  // --- Album / desktop decor (S10-M1; `01 §8.6` / FR-13-6) ---------------
  'album.photos.title': 'My album',
  'album.photos.empty': 'No photos yet — go travel and send a postcard back.',
  'album.frame.hint': 'Buy "{frame}" in the shop to display a photo as a desktop frame.',
  'decor.title': 'Desktop decor',
  'decor.pick.title': 'Pick an ornament',
  'decor.pick.empty': 'No ornaments yet — buy one in the "Ornaments" shop category.',
  'decor.slots.title': '5 desktop slots',
  'decor.slot.empty': 'Slot {n} (empty)',

  // --- Reason card (S10-M2; `01 §6.12` neglect explainability) ------------
  'reason.title': 'Why is she feeling down?',
  'reason.pressure': 'Neglect {p}/{cap}',
  'reason.none': 'She is in a good mood now ({state}); keep it up!',
  'reason.dir.faster': 'speeds neglect ×{w}',
  'reason.dir.slower': 'eases ×{w}',

  // --- Data --------------------------------------------------------------
  'data.save.section': 'Save file',
  'data.save.path': 'Save path',
  'data.save.state.fresh': 'Fresh save',
  'data.save.state.loaded': 'Healthy',
  'data.save.state.recovered': 'Recovered from backup',
  'data.save.state.isolated': 'Corrupt save isolated; defaults rebuilt',
  'data.save.state.future': 'Newer save version isolated',
  'data.save.state.migrationPending': 'Pending migration (v1 save)',
  'data.save.writable': 'Writable',
  'data.save.readonly': 'Read-only (no writes)',
  'data.save.backups': 'Available backups',
  'data.save.import': 'Import save',
  'data.save.import.hint': 'Restore from save.json.bak / save.json.v1bak / save.corrupt.*',
  'data.save.import.done': 'Imported and reloaded',
  'data.save.import.none': 'No usable backup found',
  'data.reset.section': 'Reset',
  'data.reset.label': 'Reset all data',
  'data.reset.confirm.title': 'Reset all data?',
  'data.reset.confirm.body':
    'Stats, coins, inventory, activities, skills, album, achievements and settings all return to defaults. This cannot be undone.',
  'data.reset.done': 'Reset to the default save',
  'data.session.section': 'Emotion fallback',
  'data.session.reset': 'Reset emotion',
  'data.session.reset.hint': 'Force-clears the L5 runaway state and walks her back',
  'data.session.recall': 'Bring {name} back',

  // --- Achievements ------------------------------------------------------
  'achievement.section': 'Achievements',
  'achievement.progress': '{done}/{total} unlocked',
  'achievement.locked': 'Locked',
  'achievement.placeholder':
    'The achievements skeleton is ready; definitions and progress arrive with S8.',

  // --- Tray / dialogs ----------------------------------------------------
  'dialog.quit.title': 'Quit {name}?',
  'dialog.quit.body': 'Your progress is saved before quitting.',
  'dialog.saveCorrupt.title': 'Save file was corrupt and has been recovered',
  'dialog.saveCorrupt.body':
    'The original save was isolated; import it from the Data tab in Settings.',
  'tray.settings': 'Settings',
  'tray.about': 'About',
  'tray.quit': 'Quit',
};
