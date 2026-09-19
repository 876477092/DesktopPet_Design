/**
 * 商城目录展示面（S10-M1；`01 §8.5` 商城 / `02 §5.18`）。
 *
 * ## 为什么前端静态导入 `resources/config/shop.json`
 * 目录是**静态配置**（30 件商品的展示名 / 价格 / 分类），随版本打包；Rust 侧
 * `dp-economy::Catalog` 才是购买与限额的**唯一权威**（`pet_buy` 在 core-loop 事务内
 * 校验余额 / 限购 / 日顶）。本模块只承担「渲染目录」一件事：
 *   - 不做余额判断（余额来自 `pet://state.economy.coin`）；
 *   - 不做库存判断（库存来自 `pet://state.inventory`）；
 *   - 购买按钮仍由 `ShopCard` 调 `invoke('pet_buy')`，失败由后端冲正。
 *
 * 与 Rust 双读同一文件的漂移风险：目录名 / 价格本就随版本冻结，前端只展示；
 * 即便两端短暂不一致，下单权威永远在 Rust。
 */
import rawShop from '../../resources/config/shop.json';

/** 目录内单条商品（与 `shop.json` 同构；只取 UI 需要的字段）。 */
export interface ShopCatalogItem {
  /** 商品 ID（如 `FOOD_RICEBALL` / `FURN_LANTERN` / `FRAME_WOOD`）。 */
  readonly id: string;
  /** 展示名（已含 emoji）。 */
  readonly name: string;
  /** 分类（food / groom / toy / furniture / clothing / textbook / coupon / frame）。 */
  readonly category: string;
  /** 心币价格。 */
  readonly price: number;
  /** 解锁文案（空串 = 无条件）。 */
  readonly unlockText: string;
}

/** 原始 JSON 条目（宽松读取，脏项丢弃）。 */
interface RawShopItem {
  id?: unknown;
  name?: unknown;
  category?: unknown;
  price?: unknown;
  unlock?: { text?: unknown };
}

function asString(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback;
}

function asNumber(v: unknown, fallback = 0): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}

/** 把 `shop.json` 规范化为展示目录（脏项丢弃；永不抛错）。 */
function normalize(raw: unknown): ShopCatalogItem[] {
  const root = (raw ?? {}) as { items?: unknown };
  if (!Array.isArray(root.items)) {
    return [];
  }
  const out: ShopCatalogItem[] = [];
  for (const item of root.items as RawShopItem[]) {
    if (item === null || typeof item !== 'object') {
      continue;
    }
    const id = asString(item.id);
    if (id.length === 0) {
      continue;
    }
    out.push({
      id,
      name: asString(item.name, id),
      category: asString(item.category),
      price: Math.max(0, Math.trunc(asNumber(item.price))),
      unlockText: asString(item.unlock?.text),
    });
  }
  return out;
}

/** 全量商品目录（构建期静态打包）。 */
export const SHOP_CATALOG: readonly ShopCatalogItem[] = normalize(rawShop);

/**
 * 是否为「桌面装饰」可摆放商品（`usage=place`；`01 FR-13-6`）。
 *
 * 口径：`shop.json` 用 `category: "furniture"` 标记可摆放摆件；相框（`frame`）
 * 归相册页「相框使用」，不进桌面 5 槽。
 */
export function isPlaceableDecor(item: ShopCatalogItem): boolean {
  return item.category === 'furniture';
}

/** 目录中所有可摆放装饰物（相册页装饰槽选品用）。 */
export function placeableDecorItems(): readonly ShopCatalogItem[] {
  return SHOP_CATALOG.filter(isPlaceableDecor);
}

/** 按 ID 取商品（未命中返回 `null`）。 */
export function findShopItem(id: string): ShopCatalogItem | null {
  return SHOP_CATALOG.find((item) => item.id === id) ?? null;
}
