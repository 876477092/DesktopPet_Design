import React from 'react';

/**
 * 明信片展示件（S10-M2；`01 §8.4` 旅游明信片；FR-13）。
 *
 * 纯展示：旅游返回 / 途中寄回的明信片，在相册页作为照片条目渲染。本组件不产生
 * 明信片、不调 IPC——数据由后端结算后落 `save.album`，经快照只读下发。
 *
 * 尺寸口径：`activities.json.postcard.sizePx = [160, 220]`（竖版）。
 */
export interface PostcardProps {
  /** 明信片标题（如目的地名）。 */
  title: string;
  /** 正文（已渲染好的文案；空则占位）。 */
  body?: string;
  /** 邮戳 / 日期文案（如「海边 · 第 1 天」）。 */
  stamp?: string;
}

/** 明信片（竖版卡片，邮票角标 + 正文）。 */
export function Postcard({ title, body, stamp }: PostcardProps): React.ReactElement {
  return (
    <figure className="dp-postcard" data-testid="postcard">
      <figcaption className="dp-postcard-title">{title}</figcaption>
      {stamp !== undefined && <span className="dp-postcard-stamp">{stamp}</span>}
      <blockquote className="dp-postcard-body">{body ?? ''}</blockquote>
    </figure>
  );
}

export default Postcard;
