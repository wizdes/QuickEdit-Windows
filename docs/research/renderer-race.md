# Renderer race (M1)

Winner: **arm a**, DirectWrite drawn to a bitmap and copied to the window with BitBlt, with no GPU. It has the lowest first frame of the arms that meet the typing budget, and it is the only arm that meets that budget.

Root measured these on 2026-11-01 in real windows on one desktop PC: an 8-core CPU, a discrete GPU, a 60 Hz display, Windows build 26300. Other builders were running, so the numbers are noisy. A repeat on a quiet PC is part of M15.

| Arm | Path | First frame, median (p95) | Key to screen, p50 (p95) | In-app time, p50 | Typing budget |
| --- | --- | --- | --- | --- | --- |
| a | DirectWrite to a bitmap, BitBlt | 24.0 ms (33.2) | 14.0 ms (32.8) | 1.84 ms | met |
| b | Direct2D on a flip-model swap chain | 179.0 ms (232.3) | 23.9 ms (38.7) | 0.30 ms | missed |
| c | Direct3D 11 glyph atlas, waitable swap chain | 174.1 ms (206.8) | 21.8 ms (38.6) | 0.13 ms | missed |
| d | Arm a's first frame, then arm c's path on a composition visual | 24.2 ms (30.1), GPU takes over at 191.3 ms (238.0) | 22.3 ms (38.6) | 0.14 ms | missed |

- **Budget:** key to screen p50 under 25 ms and p95 under 33 ms, with no lost keys. No arm lost a key (120 keys each).
- **First frame:** 30 warm launches, interleaved, from just before `CreateProcess` to the first present or BitBlt returning. Cold launches were not measured, because emptying the standby list needs an elevated shell.
- **Key to screen:** from just before `SendInput` to the captured frame's `LastPresentTime`, through DXGI Desktop Duplication. It excludes monitor delay, and capture may keep DWM from using a faster path, so it describes the composed path. PresentMon has not cross-checked it.

## Why arm a

- Arm a's in-app time is the highest (1.84 ms), but its key-to-screen time is the lowest. So the delay in the GPU arms comes after the app hands over the frame, and presenting matters more than drawing here.
- Loading the GPU driver costs about 150 ms at launch. In the session probe, creating a hardware Direct3D device alone took 219 ms in one run.

## Open risk

Arm a draws on the CPU, so a full redraw grows with the window's pixels. M5 must keep a full 1920 x 1080 redraw under 8 ms and use dirty regions, so that typing and scrolling stay cheap.

Run it with: `qe-perf launch` and `qe-perf typing`, from `tools/perf`. Raw results stay out of Git, in `bench-results/`.
