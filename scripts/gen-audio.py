#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""gen-audio.py —— 生成 `assets/audio/*.ogg`（S4-M6 交付物，`02 §7.7-4` / `01 §9.2`）。

由来：S4-M6 卡片「前置检查（2026-09-13 补登，P3-4）」要求逐条核对音频资产的
**来源（自制/已授权）与体积预算（≤200KB/条）**。本项目尚未引入正式音频资源，
故本脚本以**纯合成**方式产出**自制**占位音效（无第三方素材、无采样库、无版权风险），
规格与 `02 §7.7-4` 逐条对齐：

  - 容器/编码：OGG Vorbis（`soundfile` → libsndfile）；
  - 采样率：44100 Hz；
  - 响度：统一归一化 **−16 LUFS**（K 加权 + 400ms 块门控，按 BS.1770 定义近似）；
  - 体积：每条 ≤200KB（脚本末尾逐条断言，超出即非零退出）。

用法（构建期工具，不参与运行期；C1：脚本内无盘符字面量）：

    <venv>/python.exe scripts/gen-audio.py            # 写入 assets/audio
    <venv>/python.exe scripts/gen-audio.py --check    # 只校验既有产物

依赖：numpy、soundfile（`soundfile` 自带 libsndfile，含 Vorbis 编码器）。
**运行期零依赖**：本脚本只在开发/构建机运行，不进产物、不进 CI 必跑路径（P2 可加）。
"""

from __future__ import annotations

import argparse
import os
import sys

import numpy as np
import soundfile as sf

SR = 44100
# `02 §7.7-4`：单条 ≤200KB
MAX_BYTES = 200 * 1024
# `02 §7.7-4`：统一归一化 −16 LUFS
TARGET_LUFS = -16.0
# Vorbis 编码质量（短音效下 0.4 已远低于体积上限，兼顾音质）
VORBIS_QUALITY = 0.4
DEFAULT_OUT = os.path.join("assets", "audio")


# ---------------------------------------------------------------------------
# K 加权响度（BS.1770 定义的双二阶滤波 + 400ms 块门控）
# ---------------------------------------------------------------------------


def _biquad(x: np.ndarray, b: np.ndarray, a: np.ndarray) -> np.ndarray:
    """直接型 I 双二阶滤波（零初值；音效短、无稳态误差顾虑）。"""
    y = np.zeros_like(x)
    x1 = x2 = y1 = y2 = 0.0
    for i in range(x.size):
        v = b[0] * x[i] + b[1] * x1 + b[2] * x2 - a[1] * y1 - a[2] * y2
        x2, x1 = x1, x[i]
        y2, y1 = y1, v
        y[i] = v
    return y


def _high_shelf(f0: float, gain_db: float, q: float) -> tuple[np.ndarray, np.ndarray]:
    """RBJ 高架滤波器（BS.1770 stage 1）。"""
    a = 10.0 ** (gain_db / 40.0)
    w = 2.0 * np.pi * f0 / SR
    alpha = np.sin(w) / (2.0 * q)
    cosw = np.cos(w)
    b0 = a * ((a + 1) + (a - 1) * cosw + 2 * np.sqrt(a) * alpha)
    b1 = -2 * a * ((a - 1) + (a + 1) * cosw)
    b2 = a * ((a + 1) + (a - 1) * cosw - 2 * np.sqrt(a) * alpha)
    a0 = (a + 1) - (a - 1) * cosw + 2 * np.sqrt(a) * alpha
    a1 = 2 * ((a - 1) - (a + 1) * cosw)
    a2 = (a + 1) - (a - 1) * cosw - 2 * np.sqrt(a) * alpha
    return np.array([b0, b1, b2]) / a0, np.array([1.0, a1 / a0, a2 / a0])


def _high_pass(f0: float, q: float) -> tuple[np.ndarray, np.ndarray]:
    """RBJ 高通滤波器（BS.1770 stage 2）。"""
    w = 2.0 * np.pi * f0 / SR
    alpha = np.sin(w) / (2.0 * q)
    cosw = np.cos(w)
    b0 = (1 + cosw) / 2
    b1 = -(1 + cosw)
    b2 = (1 + cosw) / 2
    a0 = 1 + alpha
    a1 = -2 * cosw
    a2 = 1 - alpha
    return np.array([b0, b1, b2]) / a0, np.array([1.0, a1 / a0, a2 / a0])


_SHELF = _high_shelf(1681.97, 3.999, 0.7071)
_HIGHPASS = _high_pass(38.1358, 0.5)


def loudness_lufs(x: np.ndarray) -> float:
    """按 BS.1770 口径测响度（K 加权 + 400ms 块 / 75% 重叠 + 绝对门限 −70 LUFS）。

    单声道素材按单声道（不叠加 +3dB 声道权重）。
    """
    y = _biquad(x.astype(np.float64), *_SHELF)
    y = _biquad(y, *_HIGHPASS)
    block = int(0.4 * SR)
    hop = block // 4
    if y.size < block:
        y = np.pad(y, (0, block - y.size))
    idx = list(range(0, y.size - block + 1, hop))
    if not idx:
        idx = [0]
    z = np.array([np.mean(y[i : i + block] ** 2) for i in idx])
    z = np.maximum(z, 1e-12)
    gated = z[-0.691 + 10.0 * np.log10(z) > -70.0]
    if gated.size == 0:
        return -np.inf
    return float(-0.691 + 10.0 * np.log10(np.mean(gated)))


def _soft_limit(x: np.ndarray, ceiling: float = 0.99) -> np.ndarray:
    """tanh 软限幅：小信号近似线性（不改响度），只把峰值压到 ceiling 以内。"""
    peak = float(np.max(np.abs(x))) if x.size else 0.0
    if peak <= ceiling:
        return x
    return ceiling * np.tanh(x / ceiling)


def normalize_lufs(x: np.ndarray, target: float = TARGET_LUFS, passes: int = 4) -> np.ndarray:
    """迭代归一化到目标 LUFS，同时用软限幅把峰值约束在 0.99 以内。

    单次「加增益 + 硬降幅」会让高峰值素材掉响度，故改为迭代：每轮测量 → 补增益
    → 软限幅，收敛在同一目标上（残差 <0.2 LUFS 即停）。
    """
    y = x
    for _ in range(passes):
        current = loudness_lufs(y)
        if not np.isfinite(current):
            break
        err = target - current
        if abs(err) < 0.2:
            break
        y = y * (10.0 ** (err / 20.0))
        y = _soft_limit(y)
    peak = float(np.max(np.abs(y))) if y.size else 0.0
    if peak > 0.999:
        y = y * (0.999 / peak)
    return y


# ---------------------------------------------------------------------------
# 合成原语
# ---------------------------------------------------------------------------


def _t(dur_ms: float) -> np.ndarray:
    return np.arange(int(SR * dur_ms / 1000.0)) / SR


def env_ad(n: int, attack_ms: float, decay_ms: float, curve: float = 2.0) -> np.ndarray:
    """攻击-衰减包络（指数衰减）。"""
    t = np.arange(n) / SR
    a = max(attack_ms / 1000.0, 1e-4)
    atk = np.clip(t / a, 0.0, 1.0)
    d = max(decay_ms / 1000.0, 1e-4)
    dec = np.exp(-np.clip((t - a) / d, 0.0, None) * curve)
    return atk * dec


def tone(freq: float, dur_ms: float, attack_ms: float = 5.0, decay_ms: float | None = None) -> np.ndarray:
    t = _t(dur_ms)
    wave = np.sin(2 * np.pi * freq * t)
    return wave * env_ad(wave.size, attack_ms, decay_ms if decay_ms is not None else dur_ms * 0.5)


def saw(freq: float, dur_ms: float, attack_ms: float = 5.0) -> np.ndarray:
    t = _t(dur_ms)
    ph = (freq * t) % 1.0
    wave = 2.0 * ph - 1.0
    return wave * env_ad(wave.size, attack_ms, dur_ms * 0.6)


def sweep(f0: float, f1: float, dur_ms: float, attack_ms: float = 5.0) -> np.ndarray:
    t = _t(dur_ms)
    k = (f1 - f0) / max(dur_ms / 1000.0, 1e-6)
    phase = 2 * np.pi * (f0 * t + 0.5 * k * t * t)
    wave = np.sin(phase)
    return wave * env_ad(wave.size, attack_ms, dur_ms * 0.6)


def noise(dur_ms: float, attack_ms: float = 3.0, seed: int = 0, decay_ms: float | None = None) -> np.ndarray:
    rng = np.random.default_rng(seed)
    n = int(SR * dur_ms / 1000.0)
    wave = rng.standard_normal(n)
    return wave * env_ad(n, attack_ms, decay_ms if decay_ms is not None else dur_ms * 0.5)


def lowpass(x: np.ndarray, cutoff: float) -> np.ndarray:
    """一阶 IIR 低通（音色整形，非精确滤波）。"""
    a = max(0.0, min(1.0, 2.0 * np.pi * cutoff / SR))
    y = np.zeros_like(x)
    prev = 0.0
    for i in range(x.size):
        prev += a * (x[i] - prev)
        y[i] = prev
    return y


def highpass(x: np.ndarray, cutoff: float) -> np.ndarray:
    return x - lowpass(x, cutoff)


def bell(freq: float, dur_ms: float, decay_ms: float | None = None, partials: int = 3) -> np.ndarray:
    n = int(SR * dur_ms / 1000.0)
    out = np.zeros(n)
    d = decay_ms if decay_ms is not None else dur_ms * 0.35
    for k, gain in enumerate((1.0, 0.45, 0.22)[:partials], start=1):
        out += gain * tone(freq * k, dur_ms, 1.0, d)
    return out


def arpeggio(freqs: list[float], note_ms: float, decay_ms: float = 120.0) -> np.ndarray:
    out = np.zeros(int(SR * (note_ms * len(freqs)) / 1000.0))
    step = int(SR * note_ms / 1000.0)
    for i, f in enumerate(freqs):
        note = tone(f, note_ms, 2.0, decay_ms)
        out[i * step : i * step + note.size] += note[: max(0, out.size - i * step)]
    return out


def place(base: np.ndarray, clip: np.ndarray, at_ms: float, gain: float = 1.0) -> np.ndarray:
    i = int(SR * at_ms / 1000.0)
    end = min(base.size, i + clip.size)
    if i < base.size:
        base[i:end] += clip[: end - i] * gain
    return base


def pad(x: np.ndarray, dur_ms: float) -> np.ndarray:
    n = int(SR * dur_ms / 1000.0)
    if x.size >= n:
        return x[:n]
    return np.pad(x, (0, n - x.size))


# ---------------------------------------------------------------------------
# Cue 定义（与 dp-audio::bus::AudioCue 一一对应；`01 §9.2` 清单）
# ---------------------------------------------------------------------------


def build(name: str) -> np.ndarray:
    """按 cue 名合成波形（自制；纯参数化，无采样素材）。"""
    if name == "move_step_slow":
        x = lowpass(noise(200, 1.0, 1), 400) * 0.35
        return pad(place(x, tone(180, 180, 2.0, 70.0), 0.0), 240)
    if name == "move_step_fast":
        x = lowpass(noise(130, 1.0, 2), 700) * 0.35
        return pad(place(x, tone(260, 120, 1.0, 45.0), 0.0), 170)
    if name == "move_jump":
        return pad(sweep(300, 900, 320, 6.0), 340)
    if name == "move_land":
        x = lowpass(noise(140, 1.0, 3), 300) * 0.4
        return pad(place(x, tone(120, 260, 2.0, 120.0, ), 0.0), 290)
    if name == "move_wind":
        return pad(lowpass(noise(700, 90.0, 4), 1800) * 0.8, 720)
    if name == "interact_hi":
        return pad(sweep(600, 900, 300, 8.0), 320)
    if name == "interact_heart":
        return pad(bell(1046.5, 520, 160.0), 540)
    if name == "interact_heartbeat":
        x = np.zeros(int(SR * 0.9))
        place(x, lowpass(tone(60, 200, 3.0, 90.0), 200), 0.0, 1.0)
        place(x, lowpass(tone(55, 220, 3.0, 100.0), 200), 280, 0.8)
        return x
    if name == "interact_hehe":
        x = np.zeros(int(SR * 0.4))
        for i in range(4):
            place(x, tone(520 + i * 20, 70, 2.0, 40.0), i * 85.0, 0.8)
        return x
    if name == "interact_yaa":
        return pad(sweep(900, 400, 480, 6.0), 500)
    if name == "interact_hmph":
        return pad(saw(220, 300, 3.0), 320)
    if name == "interact_stomp":
        x = lowpass(noise(120, 1.0, 5), 250) * 0.45
        return pad(place(x, lowpass(tone(80, 220, 2.0, 100.0), 250), 0.0), 240)
    if name == "interact_magic":
        return pad(arpeggio([523.25, 659.25, 783.99, 1046.5], 180, 150.0), 800)
    if name == "emotion_hum":
        return pad(arpeggio([392.0, 440.0, 523.25], 380, 260.0), 1200)
    if name == "emotion_sob":
        x = np.zeros(int(SR * 0.9))
        for i in range(3):
            place(x, sweep(520 - i * 40, 340 - i * 30, 260, 20.0), i * 300.0, 0.85)
        return x
    if name == "emotion_sigh":
        return pad(sweep(300, 180, 780, 60.0) + lowpass(noise(780, 90.0, 7), 1200) * 0.25, 800)
    if name == "emotion_yawn":
        return pad(sweep(400, 250, 1100, 150.0), 1120)
    if name == "emotion_snore":
        x = np.zeros(int(SR * 1.4))
        for i in range(2):
            place(x, saw(92, 430, 60.0), i * 700.0, 0.9)
            place(x, lowpass(noise(180, 40.0, 8 + i), 900), i * 700.0 + 430, 0.35)
        return x
    if name == "emotion_anger":
        base = saw(150, 700, 12.0)
        t = np.arange(base.size) / SR
        trem = 0.65 + 0.35 * np.sin(2 * np.pi * 22 * t)
        return pad(base * trem, 720)
    if name == "emotion_surprise":
        return pad(sweep(700, 1400, 380, 4.0), 400)
    if name == "emotion_cheer":
        x = tone(523.25, 900, 8.0, 420.0) + tone(659.25, 900, 8.0, 420.0) * 0.7
        x += tone(783.99, 900, 8.0, 420.0) * 0.55
        x += pad(arpeggio([1046.5, 1318.5], 150, 120.0), 900) * 0.6
        return pad(x, 900)
    if name == "emotion_door_close":
        x = highpass(noise(120, 1.0, 9), 1500) * 0.4
        return pad(place(x, lowpass(tone(110, 320, 3.0, 150.0), 300), 40.0), 400)
    if name == "emotion_steps_fade":
        x = np.zeros(int(SR * 1.2))
        for i in range(4):
            place(x, lowpass(noise(110, 1.0, 10 + i), 500), i * 280.0, 1.0 - i * 0.22)
        return x
    if name == "need_growl":
        x = np.zeros(int(SR * 0.9))
        for i in range(3):
            place(x, lowpass(saw(120 - i * 8, 240, 30.0), 400), i * 300.0, 0.9)
        return x
    if name == "need_bite":
        x = np.zeros(int(SR * 0.4))
        place(x, tone(700, 90, 1.0, 40.0), 0.0, 0.9)
        place(x, tone(760, 90, 1.0, 40.0), 170.0, 0.8)
        return x
    if name == "need_burp":
        return pad(sweep(150, 90, 340, 8.0) + lowpass(noise(340, 20.0, 11), 800) * 0.3, 360)
    if name == "need_bubble":
        x = np.zeros(int(SR * 0.9))
        for i in range(6):
            place(x, tone(400 + i * 130, 90, 1.0, 45.0), i * 130.0, 0.85)
        return x
    if name == "need_shake":
        base = highpass(noise(500, 10.0, 12), 900)
        t = np.arange(base.size) / SR
        flutter = 0.7 + 0.3 * np.sin(2 * np.pi * 9 * t)
        return pad(base * flutter, 520)
    if name == "activity_dress":
        return pad(highpass(noise(350, 8.0, 13), 1500), 370)
    if name == "activity_door":
        return pad(sweep(200, 260, 600, 30.0) + lowpass(noise(600, 40.0, 14), 1000) * 0.3, 620)
    if name == "activity_coin":
        x = np.zeros(int(SR * 0.6))
        place(x, bell(1568.0, 320, 150.0, 2), 0.0, 1.0)
        place(x, bell(2093.0, 320, 150.0, 2), 190.0, 0.8)
        return x
    if name == "activity_luggage":
        base = lowpass(noise(900, 60.0, 15), 700) * 0.5
        x = np.zeros(int(SR * 0.9))
        for i in range(8):
            place(x, tone(90, 60, 2.0, 30.0), i * 110.0, 0.5)
        return pad(base + x, 900)
    if name == "activity_stamp":
        x = highpass(noise(90, 1.0, 16), 1200) * 0.5
        return pad(place(x, tone(900, 160, 1.0, 70.0), 0.0), 260)
    if name == "activity_book":
        return pad(highpass(noise(500, 20.0, 17), 2200), 520)
    if name == "activity_pant":
        x = np.zeros(int(SR * 0.9))
        for i in range(3):
            place(x, lowpass(noise(240, 60.0, 18 + i), 1400), i * 300.0, 0.9 - i * 0.15)
        return x
    if name == "remind_chime":
        x = np.zeros(int(SR * 1.2))
        for i, f in enumerate((1046.5, 1318.5, 1568.0)):
            place(x, bell(f, 700, 380.0, 2), i * 180.0, 1.0)
        return x
    if name == "remind_fireworks":
        x = highpass(noise(160, 1.0, 19), 800) * 0.6
        base = pad(x, 1000)
        for i in range(7):
            place(base, tone(1600 + i * 180, 120, 2.0, 60.0), 260.0 + i * 90.0, 0.5)
        return base
    raise KeyError(f"未定义的 cue：{name}")


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------

# 与 `dp-audio::bus::AudioCue::ALL` 严格同序同集（37 条）。
CUES = [
    "move_step_slow", "move_step_fast", "move_jump", "move_land", "move_wind",
    "interact_hi", "interact_heart", "interact_heartbeat", "interact_hehe", "interact_yaa",
    "interact_hmph", "interact_stomp", "interact_magic",
    "emotion_hum", "emotion_sob", "emotion_sigh", "emotion_yawn", "emotion_snore",
    "emotion_anger", "emotion_surprise", "emotion_cheer", "emotion_door_close",
    "emotion_steps_fade",
    "need_growl", "need_bite", "need_burp", "need_bubble", "need_shake",
    "activity_dress", "activity_door", "activity_coin", "activity_luggage", "activity_stamp",
    "activity_book", "activity_pant",
    "remind_chime", "remind_fireworks",
]


def generate(out_dir: str) -> int:
    os.makedirs(out_dir, exist_ok=True)
    failures: list[str] = []
    total = 0
    print(f"[gen-audio] 输出目录：{out_dir}（{len(CUES)} 条，期望 SR={SR}，目标 {TARGET_LUFS} LUFS）")
    for name in CUES:
        raw = build(name)
        peak = float(np.max(np.abs(raw))) if raw.size else 0.0
        if peak <= 0.0:
            failures.append(f"{name}: 波形为空")
            continue
        x = normalize_lufs(raw / peak, TARGET_LUFS)
        path = os.path.join(out_dir, f"{name}.ogg")
        sf.write(path, x.astype(np.float32), SR, format="OGG", subtype="VORBIS",
                 **{"compression_level": 0.0})
        size = os.path.getsize(path)
        total += size
        measured = loudness_lufs(x)
        info = sf.info(path)
        bad = []
        if size > MAX_BYTES:
            bad.append(f"{size}B > {MAX_BYTES}B")
        if info.samplerate != SR:
            bad.append(f"SR={info.samplerate}")
        if abs(measured - TARGET_LUFS) > 1.0:
            bad.append(f"LUFS={measured:.2f}")
        flag = "  ✗ " + "; ".join(bad) if bad else ""
        print(f"  {name:24s} {size:7d}B  {info.samplerate}Hz  {measured:6.2f} LUFS{flag}")
        if bad:
            failures.append(f"{name}: {'; '.join(bad)}")
    print(f"[gen-audio] 累计 {total / 1024:.1f}KB（均 {total / max(len(CUES), 1):.0f}B/条）")
    if failures:
        for f in failures:
            print(f"[gen-audio] 失败：{f}", file=sys.stderr)
        return 1
    print("[gen-audio] 全部通过：体积 ≤200KB/条、44100Hz、−16±1 LUFS")
    return 0


def check(out_dir: str) -> int:
    failures: list[str] = []
    for name in CUES:
        path = os.path.join(out_dir, f"{name}.ogg")
        if not os.path.isfile(path):
            failures.append(f"缺少 {path}")
            continue
        size = os.path.getsize(path)
        info = sf.info(path)
        if size > MAX_BYTES:
            failures.append(f"{name}: {size}B > {MAX_BYTES}B")
        if info.samplerate != SR:
            failures.append(f"{name}: SR={info.samplerate} ≠ {SR}")
        if info.format != "OGG":
            failures.append(f"{name}: 容器={info.format} ≠ OGG")
    if failures:
        for f in failures:
            print(f"[gen-audio] --check 未通过：{f}", file=sys.stderr)
        return 1
    print(f"[gen-audio] --check 通过：{len(CUES)} 条 OGG，44100Hz，单条 ≤200KB")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 / 校验 assets/audio/*.ogg（S4-M6）")
    parser.add_argument("--out", default=DEFAULT_OUT, help="输出目录（默认 assets/audio）")
    parser.add_argument("--check", action="store_true", help="只校验既有产物，不重新生成")
    args = parser.parse_args()
    if args.check:
        return check(args.out)
    return generate(args.out)


if __name__ == "__main__":
    sys.exit(main())
