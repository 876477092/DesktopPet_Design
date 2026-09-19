import React, { useCallback } from 'react';

import { invokeCommand } from '@shared/ipc';
import { usePetSnapshot } from '../hooks/usePetSnapshot';
import { useTranslator } from '../store/useSettings';
import DecorSlotGrid from '../components/DecorSlotGrid';
import { findShopItem, placeableDecorItems } from '@shared/shopCatalog';

/**
 * 相册 + 桌面装饰 Tab（`01 §8.6` 相册 / `01 FR-13-6` 桌面装饰；S10-M1）。
 *
 * - 照片网格：读 `pet://state.album`（旅游明信片落盘的照片；形状后端决定，前端只读渲染，
 *   缺字段退化为序号占位，不假设 schema）；
 * - 装饰 5 槽：`DecorSlotGrid`，选品 = 背包 ∩ furniture 目录，写经
 *   `pet_decor_place` / `pet_decor_remove`。
 */

/** 从不透明照片条目里尽力取一个展示名（找不到则用序号）。 */
function photoTitle(entry: unknown, index: number): string {
  if (entry !== null && typeof entry === 'object' && !Array.isArray(entry)) {
    const o = entry as Record<string, unknown>;
    const name = o.name ?? o.title ?? o.label ?? o.id;
    if (typeof name === 'string' && name.length > 0) return name;
  }
  return `#${index + 1}`;
}

export function AlbumPage(): React.ReactElement {
  const t = useTranslator();
  const snap = usePetSnapshot();

  const inventory = snap?.inventory ?? [];
  const ownedDecor = placeableDecorItems()
    .map((item) => {
      const qty = inventory.find((it) => it.itemId === item.id)?.qty ?? 0;
      return qty > 0 ? { itemId: item.id, name: item.name } : null;
    })
    .filter((x): x is { itemId: string; name: string } => x !== null);

  const album = snap?.album ?? [];
  const decor = snap?.decor ?? [null, null, null, null, null];

  const place = useCallback((slot: number, itemId: string) => {
    void invokeCommand('pet_decor_place', { slot, itemId }).catch(() => undefined);
  }, []);

  const remove = useCallback((slot: number) => {
    void invokeCommand('pet_decor_remove', { slot }).catch(() => undefined);
  }, []);

  return (
    <section className="dp-page" aria-label={t('app.tab.album')}>
      <h4 className="dp-section-title">{t('album.photos.title')}</h4>
      {album.length === 0 ? (
        <p className="dp-row-hint">{t('album.photos.empty')}</p>
      ) : (
        <ul className="dp-album-grid" data-testid="album-grid">
          {album.map((entry, i) => (
            <li className="dp-album-cell" data-testid={`photo-${i}`} key={i}>
              <span className="dp-album-photo" aria-hidden="true">
                🖼
              </span>
              <span className="dp-album-caption">{photoTitle(entry, i)}</span>
            </li>
          ))}
        </ul>
      )}

      <h4 className="dp-section-title">{t('decor.title')}</h4>
      <DecorSlotGrid decor={decor} owned={ownedDecor} t={t} onPlace={place} onRemove={remove} />

      <p className="dp-row-hint" data-testid="album-hint">
        {t('album.frame.hint', { frame: findShopItem('FRAME_WOOD')?.name ?? 'FRAME_WOOD' })}
      </p>
    </section>
  );
}

export default AlbumPage;
