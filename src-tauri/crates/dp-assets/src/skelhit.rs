//! 骨骼路径命中掩码源：32×32 覆盖率表（K-14 / `02 §4.4`，S9-M2）。
//!
//! 口径（`scripts/gen-skeleton.mjs` 产物消费方）：
//!   - 每 clip 采样 8 相位，把 region attachment 投影到 **32×32** 网格；
//!   - 每相位一行主序 1bit 打包（32×32 = 1024 bit = 128 字节；8 相位 ≈ 1KB/clip，
//!     53 clip ≈ 53KB，远小于「53×4KB≈212KB」预算上限）；
//!   - 查询坐标为**归一化帧坐标** `(u, v) ∈ [0,1]²`（u 向右、v 向下，
//!     与帧路径 `FrameRenderer` 的热区坐标系一致，AC-10 复测语义对齐）；
//!   - 镜像查询与帧路径 `bit_at_mirrored` 同语义：`u' = 1 - u`（K-4）。
//!
//! 降级：非法 / 缺 clip / 缺相位 → `false`（不命中，`02 §7.4` 不崩溃）。

use serde::Deserialize;

use crate::AssetError;

/// 网格边长（`gen-skeleton.mjs` 冻结 32）。
pub const SKEL_GRID: u32 = 32;

/// 单相位打包字节数（32×32 bit / 8 = 128）。
const PHASE_BYTES: usize = (SKEL_GRID as usize * SKEL_GRID as usize) / 8;

/// 单相位覆盖率位图（行主序，MSB-first，与 `mask::HitMask` 同打包口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Phase {
    cells: Vec<u8>,
}

impl Phase {
    /// 空相位（无任何占用）。
    fn empty() -> Self {
        Self { cells: vec![0u8; PHASE_BYTES] }
    }

    /// 由扁平化占用格索引集合构建（gen-skeleton.mjs `cells: [row*32+col, …]`）。
    fn from_indices(indices: &[u32]) -> Self {
        let mut phase = Self::empty();
        for &idx in indices {
            let i = idx as usize;
            if i < (SKEL_GRID as usize * SKEL_GRID as usize) {
                phase.cells[i >> 3] |= 0x80 >> (i & 7);
            }
        }
        phase
    }

    /// 查询归一化坐标是否命中。
    fn hit(&self, u: f32, v: f32) -> bool {
        let col = (u.clamp(0.0, 1.0) * SKEL_GRID as f32).floor() as u32;
        let row = (v.clamp(0.0, 1.0) * SKEL_GRID as f32).floor() as u32;
        // 边界钳到最后一格（u/v=1.0 落最后一行/列）。
        let col = col.min(SKEL_GRID - 1);
        let row = row.min(SKEL_GRID - 1);
        let index = (row * SKEL_GRID + col) as usize;
        (self.cells[index >> 3] >> (7 - (index & 7))) & 1 == 1
    }
}

/// 单个 clip 的全部相位。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipCoverage {
    phases: Vec<Phase>,
}

/// 全 clip 骨骼命中率表（coverage.json 反序列化结果）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkelHitTable {
    clips: std::collections::BTreeMap<String, ClipCoverage>,
}

// ---- coverage.json 反序列化中间结构（serde 容错缺字段） ----------------------

#[derive(Debug, Deserialize)]
struct CoverageDoc {
    #[serde(default)]
    clips: std::collections::BTreeMap<String, CoverageClip>,
}

#[derive(Debug, Deserialize)]
struct CoverageClip {
    #[serde(default)]
    phases: Vec<CoveragePhase>,
}

#[derive(Debug, Deserialize)]
struct CoveragePhase {
    #[serde(default)]
    cells: Vec<u32>,
}

impl SkelHitTable {
    /// 空表（任何查询都 false；降级结果）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 是否为空表。
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// clip 数量。
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    /// 相位数（缺 clip → 0）。
    pub fn phase_count(&self, clip: &str) -> usize {
        self.clips.get(clip).map(|c| c.phases.len()).unwrap_or(0)
    }

    /// 从 coverage.json 文本构建（`02 §7.4`：解析失败降级为空表，不崩溃）。
    pub fn from_coverage_json(json: &str) -> Result<Self, AssetError> {
        let doc: CoverageDoc = serde_json::from_str(json)
            .map_err(|e| AssetError::AtlasJson(format!("coverage.json 无效：{e}")))?;
        let mut clips = std::collections::BTreeMap::new();
        for (name, clip) in doc.clips {
            let phases = clip
                .phases
                .iter()
                .map(|p| Phase::from_indices(&p.cells))
                .collect();
            clips.insert(name, ClipCoverage { phases });
        }
        Ok(Self { clips })
    }

    /// 查询指定 clip / 相位的归一化坐标是否命中。
    ///
    /// `phase` 越界时取最后一相位（保守：动画循环末相位）；clip 缺失 → false。
    pub fn hit(&self, clip: &str, phase: usize, u: f32, v: f32) -> bool {
        let Some(cov) = self.clips.get(clip) else {
            return false;
        };
        if cov.phases.is_empty() {
            return false;
        }
        let idx = phase.min(cov.phases.len() - 1);
        cov.phases[idx].hit(u, v)
    }

    /// 镜像查询（K-4：与帧路径 `bit_at_mirrored` 同语义 `u' = 1 - u`）。
    pub fn hit_mirrored(&self, clip: &str, phase: usize, u: f32, v: f32) -> bool {
        self.hit(clip, phase, 1.0 - u, v)
    }
}

/// 把覆盖率表打包为前端热区可用的紧凑结构（预留 S9-M4 bbox 复测导出）。
///
/// 返回每 clip 的相位数与总占用格数（调试/QA 取证用）。
pub fn summarize(table: &SkelHitTable) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    for (name, cov) in &table.clips {
        let occupied: usize = cov
            .phases
            .iter()
            .map(|p| p.cells.iter().map(|b| b.count_ones() as usize).sum::<usize>())
            .sum();
        out.push((name.clone(), cov.phases.len(), occupied));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个单 clip、单相位、占 (row,col)=(1,1) 一格的 coverage.json 文本。
    fn doc_with_one_cell(clip: &str, flat_index: u32) -> String {
        format!(
            r#"{{ "gridSize": 32, "clips": {{ "{clip}": {{ "phases": [ {{ "cells": [{flat_index}] }} ] }} }} }}"#
        )
    }

    #[test]
    fn builds_from_json_and_hits_cell() {
        // flat index = row*32+col；取 row=1,col=2 → 34。
        let table = SkelHitTable::from_coverage_json(&doc_with_one_cell("wave", 34)).expect("合法 json");
        assert_eq!(table.len(), 1);
        assert_eq!(table.phase_count("wave"), 1);
        // cell (row=1,col=2)：u∈[2/32,3/32)、v∈[1/32,2/32) 命中。
        assert!(table.hit("wave", 0, 2.5 / 32.0, 1.5 / 32.0));
        // 紧邻格不命中。
        assert!(!table.hit("wave", 0, 3.5 / 32.0, 1.5 / 32.0));
    }

    #[test]
    fn missing_clip_degrades_to_false() {
        let table = SkelHitTable::from_coverage_json(&doc_with_one_cell("wave", 0)).unwrap();
        assert!(!table.hit("nope", 0, 0.5, 0.5));
        assert!(table.is_empty() == false);
    }

    #[test]
    fn mirrored_query_uses_one_minus_u() {
        // 占 col=0（左缘）一格：u 小处命中，u 大处不命中。
        let table = SkelHitTable::from_coverage_json(&doc_with_one_cell("wave", 0)).unwrap();
        assert!(table.hit("wave", 0, 0.5 / 32.0, 0.5 / 32.0)); // 原图左上命中
        // 镜像后同一点应映射到右缘（col=31），而我们的格子在 col=0 → 不命中。
        assert!(!table.hit_mirrored("wave", 0, 0.5 / 32.0, 0.5 / 32.0));
        // 右缘镜像回左缘应命中。
        assert!(table.hit_mirrored("wave", 0, 31.5 / 32.0, 0.5 / 32.0));
    }

    #[test]
    fn phase_out_of_range_falls_back_to_last() {
        let table = SkelHitTable::from_coverage_json(&doc_with_one_cell("wave", 0)).unwrap();
        // phase=999 → 取最后一相位（这里只有 1 个）。
        assert!(table.hit("wave", 999, 0.5 / 32.0, 0.5 / 32.0));
    }

    #[test]
    fn invalid_json_degrades() {
        assert!(SkelHitTable::from_coverage_json("not json").is_err());
        assert!(SkelHitTable::empty().is_empty());
    }
}
