/**
 * 右键菜单纯逻辑（S3-M6，T-10 段 · 下；`01 §8.2` 冻结口径）。
 *
 * 职责：
 *   - [`buildMenuItems`]：九项列表式菜单（3×3 行主序）的唯一真源——**菜单项不得
 *     超出 `01 §8.2` 定义范围**（卡片边界）；本阶段仅「隐藏」可实接（窗口隐藏，
 *     复用既有窗口能力），其余灰化并如实显示功能条件文案（§8.2 规格）。
 *   - [`clampMenuPlacement`]：菜单摆位钳制（锚点优先、越界回收，不越容器）。
 *
 * 纯函数零 DOM 零时钟（C3）；C2 不涉角色名。
 */

import {
  MENU_EDGE_PAD,
  MENU_HEIGHT,
  MENU_WIDTH,
  type MenuItemId,
  type MenuItemSpec,
  type MenuPlacement,
} from './layerPorts';

/**
 * 九项菜单规格（`01 §8.2` 逐项对齐；顺序 = 3×3 行主序，勿重排）。
 *
 * 灰化条件文案口径（主理人裁定）：不写死版本号，写**功能条件**——未实装系统
 * 接入后开放；文案与 §8.2「未解锁项灰化并显示条件」一致。
 */
export function buildMenuItems(): MenuItemSpec[] {
  const disabled = (id: MenuItemId, label: string, emoji: string, reason: string): MenuItemSpec => ({
    id,
    label,
    emoji,
    enabled: false,
    reason,
  });
  return [
    disabled('stroke', '抚摸', '\u{1F91A}', '哄摸流程可用后开放'),
    disabled('feed', '喂食', '\u{1F359}', '喂养功能可用后开放'),
    disabled('bath', '洗澡', '\u{1F6C1}', '洗澡功能可用后开放'),
    disabled('dispatch', '派遣', '\u{1F4BC}', '派遣功能可用后开放'),
    disabled('shop', '商城', '\u{1F3EA}', '商城开放后可用'),
    disabled('attrs', '属性', '\u{1F4CA}', '属性面板可用后开放'),
    disabled('photo', '拍照', '\u{1F4F7}', '拍照功能可用后开放'),
    disabled('settings', '设置', '\u{2699}', '设置面板可用后开放'),
    // 唯一可实接项：隐藏窗口（复用既有窗口能力；命令经 menu_command 收口）。
    { id: 'hide', label: '隐藏', emoji: '\u{1F47B}', enabled: true, reason: null },
  ];
}

/** 菜单九项 id 行主序（`01 §8.2` 冻结序；摆位/接线测试锚点）。 */
export const MENU_ITEM_ORDER: readonly MenuItemId[] = [
  'stroke',
  'feed',
  'bath',
  'dispatch',
  'shop',
  'attrs',
  'photo',
  'settings',
  'hide',
];

/**
 * 菜单摆位钳制：以命中点为菜单左上角优先，越界时向内回收（保持与命中点的
 * 可见邻近性）；容器小于菜单时钳到安全间隙（退化安全，不越界不 panic）。
 */
export function clampMenuPlacement(
  at: { readonly x: number; readonly y: number },
  container: { readonly width: number; readonly height: number },
): MenuPlacement {
  const maxX = Math.max(MENU_EDGE_PAD, container.width - MENU_WIDTH - MENU_EDGE_PAD);
  const maxY = Math.max(MENU_EDGE_PAD, container.height - MENU_HEIGHT - MENU_EDGE_PAD);
  return {
    left: Math.min(maxX, Math.max(MENU_EDGE_PAD, at.x)),
    top: Math.min(maxY, Math.max(MENU_EDGE_PAD, at.y)),
  };
}
