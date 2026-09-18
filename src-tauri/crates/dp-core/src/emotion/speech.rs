//! `emotion::speech`：口头禅改写引擎（**S7-M8**，`01 §6.16.2` / §6.16.4 / `02 §5.8`）。
//!
//! ## 本模块职责（`03 §2 S7-M8`）
//!
//!   1. **位置偏置注入**（R5）：把「无口头禅句」改写成含口头禅句，注入位置按
//!      `character.json.catchphrase.positionBias` 加权（句首 ≥70% / 句尾 / 句中 = 0，
//!      R5 禁止句中硬插）；
//!   2. **单句上限**（R2）：`maxPerSentence`（默认 1）；已含口头禅的句子**不重复注入**；
//!   3. **确定性**（C3 同口径）：注入位置以 `(text, now_ms)` 哈希加权选取，可单测；
//!   4. **零时钟**：`now_ms` 由调用方注入；token / 权重全部取自 `character.json`（C7）。
//!
//! ## 与 S4-M5 的切割
//!
//!   - 频率闸门（R1 滚动窗口 / R3 冷却）在 [`crate::emotion::lines::CatchphraseGate`]
//!     （S4-M5 交付）；本模块只做「放行后的**改写**」与「已含句子的**识别**」；
//!   - 实际接线（池 → 闸门 → 改写 → 降频换无口头禅变体）在
//!     [`crate::emotion::lines::BubblePlanner`]（S7-M8）。

use crate::config::model::CatchphraseCfg;

/// 注入位置（`01 §6.16.4` R5：句首 ≥70%，句尾次之，句中禁止）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectPosition {
    /// 句首。
    Head,
    /// 句尾。
    Tail,
    /// 句中（`positionBias.middle = 0` 时永不选中；防御性实现：无安全边界则回落句尾）。
    Middle,
}

/// 口头禅改写器（S7-M8）。
///
/// 幂等口径：文本已含口头禅 → 原样返回（不重复注入，R2）；token 为空 → 原样返回
/// （诚实降级，不 panic）。
#[derive(Debug, Clone)]
pub struct SpeechRewriter {
    token: String,
    head_weight: u64,
    tail_weight: u64,
    middle_weight: u64,
    max_per_sentence: u32,
}

impl SpeechRewriter {
    /// 由 `character.json.catchphrase` 构造。
    #[must_use]
    pub fn from_cfg(cfg: &CatchphraseCfg) -> Self {
        let bias = &cfg.position_bias;
        Self {
            token: cfg.token.clone(),
            head_weight: weight_permille(bias.head),
            tail_weight: weight_permille(bias.tail),
            middle_weight: weight_permille(bias.middle),
            max_per_sentence: cfg.max_per_sentence.max(1),
        }
    }

    /// 口头禅 token（只读；诊断 / 单测）。
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 文本是否已含口头禅（R2 不重复注入；`validate` 也按此口径统计池分布）。
    #[must_use]
    pub fn contains_token(&self, text: &str) -> bool {
        !self.token.is_empty() && text.contains(self.token.as_str())
    }

    /// 单句上限（R2；构造时钳到 ≥1）。
    #[must_use]
    pub fn max_per_sentence(&self) -> u32 {
        self.max_per_sentence
    }

    /// 按位置偏置注入口头禅（确定性：以 `(text, now_ms)` 哈希加权选位）。
    ///
    /// 已含口头禅 / token 为空 → 原样返回（不重复注入、不 panic）。
    #[must_use]
    pub fn inject(&self, text: &str, now_ms: i64) -> String {
        if self.contains_token(text) || self.token.is_empty() {
            return text.to_string();
        }
        match self.pick_position(text, now_ms) {
            InjectPosition::Head => format!("{}{}", self.token, text),
            InjectPosition::Tail => format!("{}{}", text, self.token),
            InjectPosition::Middle => self.inject_middle(text),
        }
    }

    /// 加权选位（确定性；全零权重回退句首，避免「永不注入」的静默失效）。
    fn pick_position(&self, text: &str, now_ms: i64) -> InjectPosition {
        let total = self.head_weight + self.tail_weight + self.middle_weight;
        if total == 0 {
            return InjectPosition::Head;
        }
        let h = mix_hash(text, now_ms) % total;
        if h < self.head_weight {
            InjectPosition::Head
        } else if h < self.head_weight + self.tail_weight {
            InjectPosition::Tail
        } else {
            InjectPosition::Middle
        }
    }

    /// 句中注入（防御性分支，`middle=0` 时不会走到）：在句中第一个自然边界
    /// （标点 / 空格）之后插入；无边界 → 回落句尾（R5 禁止硬插的兜底）。
    fn inject_middle(&self, text: &str) -> String {
        let half = text.len() / 2;
        let mut insert_at = None;
        for (i, c) in text.char_indices() {
            if i < half {
                continue;
            }
            if matches!(c, '，' | '。' | '！' | '？' | '、' | ' ' | '\u{3000}') {
                insert_at = Some(i + c.len_utf8());
                break;
            }
        }
        match insert_at {
            Some(idx) => {
                let mut out = String::with_capacity(text.len() + self.token.len());
                out.push_str(&text[..idx]);
                out.push_str(&self.token);
                out.push_str(&text[idx..]);
                out
            }
            None => format!("{}{}", text, self.token),
        }
    }
}

/// 位置偏置权重 → 千分位整数（钳到 `[0, 1000]`；`NaN` / 非正数按 0，容错不 panic）。
fn weight_permille(v: f32) -> u64 {
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    (v * 1000.0).round().clamp(0.0, 1000.0) as u64
}

/// 确定性终混（与 `lines::hash_index` 同族：FNV-1a 喂文本 → SplitMix64 终混）。
fn mix_hash(text: &str, now_ms: i64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= now_ms as u64;
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{CatchphraseCfg, PositionBiasCfg};

    /// 默认偏置（head 0.7 / tail 0.3 / middle 0）下，注入结果里 token 出现次数为 1。
    #[test]
    fn inject_adds_token_once() {
        let rw = SpeechRewriter::from_cfg(&CatchphraseCfg::default());
        let out = rw.inject("今天天气真好", 0);
        assert_eq!(out.matches(rw.token()).count(), 1, "R2：单句 ≤1 次");
        assert!(out.contains("今天天气真好"), "原句内容保留");
    }

    /// 已含口头禅 → 不重复注入（R2）。
    #[test]
    fn inject_does_not_double_on_baked_token() {
        let rw = SpeechRewriter::from_cfg(&CatchphraseCfg::default());
        let out = rw.inject("心心好开心！", 0);
        assert_eq!(out, "心心好开心！");
    }

    /// 空 token → 原样返回（诚实降级）。
    #[test]
    fn inject_noop_when_token_empty() {
        let mut cfg = CatchphraseCfg::default();
        cfg.token.clear();
        let rw = SpeechRewriter::from_cfg(&cfg);
        assert_eq!(rw.inject("你好呀", 0), "你好呀");
        assert!(!rw.contains_token("你好呀"));
    }

    /// 位置偏置：默认偏置下句首占主导（抽样 200 组确定性样本），句中恒为 0（R5）。
    #[test]
    fn position_bias_head_dominant_and_middle_never() {
        let rw = SpeechRewriter::from_cfg(&CatchphraseCfg::default());
        let mut head = 0u32;
        let mut middle = 0u32;
        for t in 0..200 {
            let out = rw.inject("这是一句普普通通的台词", t * 997);
            if out.starts_with(rw.token()) {
                head += 1;
            }
            // 句中判定：token 不在首、不在尾。
            if out.contains(rw.token())
                && !out.starts_with(rw.token())
                && !out.ends_with(rw.token())
            {
                middle += 1;
            }
        }
        assert_eq!(middle, 0, "R5：句中权重 0 → 永不句中插入");
        assert!(head >= 100, "句首应占主导（≥50%），实际 {head}/200");
    }

    /// 全零权重 → 回退句首（不静默失效）。
    #[test]
    fn zero_bias_falls_back_to_head() {
        let cfg = CatchphraseCfg { position_bias: PositionBiasCfg { head: 0.0, tail: 0.0, middle: 0.0 }, ..Default::default() };
        let rw = SpeechRewriter::from_cfg(&cfg);
        let out = rw.inject("测试一下", 42);
        assert!(out.starts_with(rw.token()));
    }

    /// 纯句尾偏置 → 全部注入在句尾。
    #[test]
    fn tail_only_bias_injects_at_tail() {
        let cfg = CatchphraseCfg { position_bias: PositionBiasCfg { head: 0.0, tail: 1.0, middle: 0.0 }, ..Default::default() };
        let rw = SpeechRewriter::from_cfg(&cfg);
        for t in 0..50 {
            let out = rw.inject("例行台词", t * 131);
            assert!(out.ends_with(rw.token()), "纯句尾偏置应在句尾，t={t}");
        }
    }

    /// 句中偏置（防御分支）：插入在边界之后且不破坏字符边界。
    #[test]
    fn middle_injection_lands_after_boundary() {
        let cfg = CatchphraseCfg { position_bias: PositionBiasCfg { head: 0.0, tail: 0.0, middle: 1.0 }, ..Default::default() };
        let rw = SpeechRewriter::from_cfg(&cfg);
        let text = "前半句好无聊，后半句也好无聊";
        let out = rw.inject(text, 7);
        assert_eq!(out.matches(rw.token()).count(), 1);
        assert!(out.ends_with(rw.token()) || out.contains("，"), "应落在边界后或回落句尾");
    }

    /// 确定性：同输入 → 同输出。
    #[test]
    fn injection_is_deterministic() {
        let rw = SpeechRewriter::from_cfg(&CatchphraseCfg::default());
        let a = rw.inject("同样的句子", 12_345);
        let b = rw.inject("同样的句子", 12_345);
        assert_eq!(a, b);
    }

    /// 单句上限钳到 ≥1（配置写 0 也按 1 处理）。
    #[test]
    fn max_per_sentence_is_clamped_at_least_one() {
        let cfg = CatchphraseCfg { max_per_sentence: 0, ..Default::default() };
        assert_eq!(SpeechRewriter::from_cfg(&cfg).max_per_sentence(), 1);
    }

    /// 权重钳制：负值 / NaN / 超界 → 不 panic 且收敛到合法值域。
    #[test]
    fn weights_are_sanitized() {
        let cfg = CatchphraseCfg { position_bias: PositionBiasCfg { head: f32::NAN, tail: -1.0, middle: 99.0 }, ..Default::default() };
        let rw = SpeechRewriter::from_cfg(&cfg);
        let out = rw.inject("健壮性检查", 1);
        assert_eq!(out.matches(rw.token()).count(), 1);
    }
}
