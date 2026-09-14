//! 图集元数据解析（atlas.json → 帧矩形）。
//!
//! atlas.json 由 `scripts/gen-atlas.mjs` 生成（`02 §7.7`：禁止手写），两端结构
//! **同构冻结**（本文件与 gen-atlas.mjs 头注释互为镜像文档）：
//!
//! ```jsonc
//! {
//!   "version": 1,
//!   "actions": [
//!     {
//!       "actionId": "ACT-M-01",
//!       "png": "ACT-M-01_idle.png",          // 图集文件名（相对同目录）
//!       "frameW": 256, "frameH": 256,        // 单帧物理尺寸（2x 导出）
//!       "columns": 8, "rows": 1,             // 横向图集，行主序
//!       "frameCount": 8,
//!       "anchor": { "x": 128, "y": 256 },    // 锚点（物理像素；默认底部中心）
//!       "hit": { "threshold": 32 },          // alpha 命中阈值（02 §7.7-3）
//!       "secondary": [                        // 次级热区（狐耳等；与 K-2 同构）
//!         { "name": "ear_l", "x": 0, "y": 0, "w": 64, "h": 64 }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! 帧矩形布局：**横向图集、行主序** —— 第 `index` 帧位于
//! `col = index % columns`、`row = index / columns`，矩形
//! `(x = col * frameW, y = row * frameH, w = frameW, h = frameH)`。

use serde::{Deserialize, Serialize};

use crate::AssetError;

/// 图集文件容器（一个图集目录汇总多动作；gen-atlas.mjs 输出顶层结构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AtlasFile {
    /// 结构版本。
    pub version: u32,
    /// 各动作的图集元数据。
    pub actions: Vec<AtlasMeta>,
}

impl Default for AtlasFile {
    fn default() -> Self {
        Self { version: 1, actions: Vec::new() }
    }
}

impl AtlasFile {
    /// 从 JSON 文本解析（含结构校验）。
    pub fn from_json_str(json: &str) -> Result<Self, AssetError> {
        let parsed: AtlasFile = serde_json::from_str(json)
            .map_err(|err| AssetError::AtlasJson(err.to_string()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// 从字节数组解析（UTF-8）。
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, AssetError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|err| AssetError::AtlasJson(format!("非 UTF-8：{err}")))?;
        Self::from_json_str(text)
    }

    /// 按动作 ID 查找元数据。
    pub fn find(&self, action_id: &str) -> Option<&AtlasMeta> {
        self.actions.iter().find(|meta| meta.action_id == action_id)
    }

    /// 结构校验（columns 为 0 / frameCount 与 columns*rows 矛盾 → Err）。
    fn validate(&self) -> Result<(), AssetError> {
        for meta in &self.actions {
            meta.validate()?;
        }
        Ok(())
    }
}

/// 锚点（物理像素；`{x:128, y:256}` = 底部中心）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AnchorCfg {
    /// 锚点 X（相对单帧左上）。
    pub x: u32,
    /// 锚点 Y。
    pub y: u32,
}

/// 主命中配置（alpha 阈值；`02 §7.7-3`：hit.threshold = 32）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HitCfg {
    /// alpha ≥ threshold 判为不透明像素。
    pub threshold: u8,
}

/// 次级热区（狐耳等；语义与 `02 §5 K-2` atlas.json.secondary 同构：悬停有效、点击落主判定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SecondaryRegionCfg {
    /// 区域名。
    pub name: String,
    /// X（相对单帧左上，物理像素）。
    pub x: u32,
    /// Y。
    pub y: u32,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
}

/// 单动作图集元数据（与 gen-atlas.mjs 输出同构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AtlasMeta {
    /// 动作 ID（`ACT-` 前缀，RV-12）。
    pub action_id: String,
    /// 图集 PNG 文件名（相对 atlas.json 同目录）。
    pub png: String,
    /// 单帧宽（物理像素）。
    pub frame_w: u32,
    /// 单帧高（物理像素）。
    pub frame_h: u32,
    /// 横向列数。
    pub columns: u32,
    /// 行数。
    pub rows: u32,
    /// 帧总数。
    pub frame_count: u32,
    /// 锚点。
    pub anchor: AnchorCfg,
    /// 主命中配置。
    pub hit: HitCfg,
    /// 次级热区列表。
    pub secondary: Vec<SecondaryRegionCfg>,
}

impl AtlasMeta {
    /// 从 JSON 文本解析单个动作元数据（裸对象，非 AtlasFile 包装）。
    pub fn from_json_str(json: &str) -> Result<Self, AssetError> {
        let parsed: AtlasMeta =
            serde_json::from_str(json).map_err(|err| AssetError::AtlasJson(err.to_string()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// 第 `index` 帧在图集中的矩形（横向图集、行主序；越界 → Err）。
    pub fn frame_rect(&self, index: u32) -> Result<Rect, AssetError> {
        if index >= self.frame_count {
            return Err(AssetError::FrameIndexOutOfRange {
                index,
                frame_count: self.frame_count,
            });
        }
        let col = index % self.columns;
        let row = index / self.columns;
        Ok(Rect {
            x: col * self.frame_w,
            y: row * self.frame_h,
            w: self.frame_w,
            h: self.frame_h,
        })
    }

    /// 结构校验。
    fn validate(&self) -> Result<(), AssetError> {
        if self.columns == 0 || self.rows == 0 {
            return Err(AssetError::AtlasJson(format!(
                "动作 {} 的 columns/rows 不得为 0",
                self.action_id
            )));
        }
        if self.frame_count == 0 || self.frame_count > self.columns * self.rows {
            return Err(AssetError::AtlasJson(format!(
                "动作 {} 的 frameCount={} 与 columns×rows={} 不符",
                self.action_id,
                self.frame_count,
                self.columns * self.rows
            )));
        }
        Ok(())
    }
}

/// 图集内子矩形（物理像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// X（相对图集左上）。
    pub x: u32,
    /// Y。
    pub y: u32,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gen-atlas.mjs 输出样例（单动作、横向 8 列）。
    const SAMPLE_FILE: &str = r#"{
        "version": 1,
        "actions": [
            {
                "actionId": "ACT-M-01",
                "png": "ACT-M-01_idle.png",
                "frameW": 256, "frameH": 256,
                "columns": 8, "rows": 1, "frameCount": 8,
                "anchor": { "x": 128, "y": 256 },
                "hit": { "threshold": 32 },
                "secondary": [ { "name": "ear_l", "x": 0, "y": 0, "w": 64, "h": 64 } ]
            },
            {
                "actionId": "ACT-E-01",
                "png": "ACT-E-01_happy_spin.png",
                "frameW": 256, "frameH": 256,
                "columns": 2, "rows": 3, "frameCount": 6,
                "anchor": { "x": 128, "y": 256 },
                "hit": { "threshold": 32 },
                "secondary": []
            }
        ]
    }"#;

    #[test]
    fn parses_gen_atlas_output() {
        let file = AtlasFile::from_json_str(SAMPLE_FILE).expect("样例应可解析");
        assert_eq!(file.version, 1);
        assert_eq!(file.actions.len(), 2);

        let meta = file.find("ACT-M-01").expect("应包含 ACT-M-01");
        assert_eq!(meta.png, "ACT-M-01_idle.png");
        assert_eq!(meta.hit.threshold, 32);
        assert_eq!(meta.anchor, AnchorCfg { x: 128, y: 256 });
        assert_eq!(meta.secondary.len(), 1);
        assert_eq!(meta.secondary[0].name, "ear_l");
    }

    #[test]
    fn frame_rect_layout_row_major_horizontal() {
        let file = AtlasFile::from_json_str(SAMPLE_FILE).expect("样例应可解析");
        let meta = file.find("ACT-M-01").expect("应包含 ACT-M-01");

        assert_eq!(meta.frame_rect(0).expect("0 应合法"), Rect { x: 0, y: 0, w: 256, h: 256 });
        assert_eq!(
            meta.frame_rect(3).expect("3 应合法"),
            Rect { x: 768, y: 0, w: 256, h: 256 }
        );
        assert_eq!(
            meta.frame_rect(7).expect("7 应合法"),
            Rect { x: 1792, y: 0, w: 256, h: 256 }
        );

        // 多行：columns=2, rows=3。
        let multi = file.find("ACT-E-01").expect("应包含 ACT-E-01");
        assert_eq!(multi.frame_rect(4).expect("4 应合法"), Rect { x: 0, y: 512, w: 256, h: 256 });
        assert_eq!(multi.frame_rect(5).expect("5 应合法"), Rect { x: 256, y: 512, w: 256, h: 256 });
    }

    #[test]
    fn frame_rect_out_of_range_is_err() {
        let file = AtlasFile::from_json_str(SAMPLE_FILE).expect("样例应可解析");
        let meta = file.find("ACT-M-01").expect("应包含 ACT-M-01");
        let err = meta.frame_rect(8).expect_err("8 应越界");
        assert!(matches!(err, AssetError::FrameIndexOutOfRange { index: 8, frame_count: 8 }));
    }

    #[test]
    fn zero_columns_is_invalid() {
        let json = r#"{
            "actionId": "ACT-X-99", "png": "x.png",
            "frameW": 256, "frameH": 256,
            "columns": 0, "rows": 1, "frameCount": 0,
            "anchor": { "x": 0, "y": 0 }, "hit": { "threshold": 32 }, "secondary": []
        }"#;
        assert!(AtlasMeta::from_json_str(json).is_err());
    }
}
