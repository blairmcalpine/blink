# Benchmarking

blink's only non-negotiable quality bar is that running it costs almost no
in-game frames. This document defines how to measure that, so numbers from
different changes and different people are comparable.

Every change that touches the hot path (capture, color conversion, encode, the
packet ring, muxing, thread priorities, or anything that allocates per frame)
must include before-and-after numbers from this procedure in its pull request.

Target: on par with NVIDIA ShadowPlay, meaning a few percent FPS loss or less.

## What you need

- A Windows machine with the NVIDIA GPU and driver you intend to report on.
- A recent [PresentMon](https://github.com/GameTechDev/PresentMon) CLI build, or
  CapFrameX if you prefer a GUI. The steps below use PresentMon.
- A GPU monitoring tool that can show per-engine utilization (for example the
  `nvidia-smi` query below, or HWiNFO). You need 3D engine and video encode
  engine separately.

Record the exact versions of everything: GPU model, driver version, Windows
build, blink commit, game build, and PresentMon version.

## Reference workload: CS2

Counter-Strike 2 is the reference game because it is CPU-bound, competitive, and
sensitive to frame pacing, which makes any added cost show up.

1. Launch CS2 in fullscreen.
2. Load a repeatable benchmark map or demo. Do not free-roam; the run must be the
   same sequence of inputs and camera movement every time.
3. Let the game settle for a minute so shaders compile and clocks stabilize.
4. Run the same configuration three times and take the median. A single run is
   noise.

Run the whole series twice, once per display configuration:

- **Native resolution**: the panel's native mode.
- **Stretched 4:3**: a 4:3 resolution such as 1280x960 in fullscreen, with the
  GPU set to scale to the full panel. This is the configuration that exercises
  `stretch_to` and the mode-change path.

For each configuration, capture two states:

- **blink off**: the baseline.
- **blink recording**: blink idle but buffering continuously into the ring, which
  is the normal running state.

Optionally add a third state, **clip save in progress**, to check that saving does
not perturb the game. It should not: saving is a remux on a low-priority thread.

## PresentMon capture

PresentMon writes a CSV for the named process. Capture the same number of seconds
per run so the runs are comparable. Run these in `cmd` or PowerShell, and keep
each command on one line (the shell continuation syntax differs between them).

```bat
PresentMon --process_name cs2.exe --output_file native-off.csv --terminate_after_seconds 120
PresentMon --process_name cs2.exe --output_file native-blinking.csv --terminate_after_seconds 120
```

Repeat with the game in the stretched 4:3 mode, naming the files accordingly
(`stretched-off.csv`, `stretched-blinking.csv`). Start PresentMon before the
benchmark sequence begins and let it run through the whole sequence.

PresentMon's flags have changed between releases; if the command above is
rejected, check `PresentMon --help` for the current spelling of the process-name
and duration options, and record the tool version you used.

If you prefer CapFrameX, record the same sequence and configurations, and export
the same statistics.

## GPU engine attribution

To show that the cost lands on NVENC rather than the 3D engine, sample engine
utilization during the run:

```bat
nvidia-smi --query-gpu=utilization.gpu,utilization.encoder,utilization.decoder --format=csv --loop-ms=500
```

Compare the 3D engine utilization between blink off and blink recording. A
correct implementation moves encoder utilization up and leaves the 3D engine
close to unchanged. Also record blink's own CPU usage, as a percentage of total
CPU, from Task Manager or `typeperf`, or by reading the process's CPU time.

## What to report

For each configuration and state, report:

- **Average FPS**
- **1% low FPS**
- **Frame time percentiles**: p50, p95, p99, and the maximum
- **blink CPU usage**: percentage of total CPU
- **GPU 3D engine utilization**: percentage, averaged over the run
- **GPU video encode engine utilization**: percentage, averaged over the run

Report the delta from baseline for each, and state the direction plainly (for
example "average FPS -1.8%, 1% low -2.4%"). Include the raw CSVs or a link to
them, and state the run count and whether the numbers are medians.

## Results template

Copy this table into the pull request description and fill it in. One block per
display configuration.

### Native resolution (`WIDTHxHEIGHT @ REFRESH`)

| Metric | blink off | blink recording | Delta |
| --- | --- | --- | --- |
| Average FPS | | | |
| 1% low FPS | | | |
| Frame time p50 (ms) | | | |
| Frame time p95 (ms) | | | |
| Frame time p99 (ms) | | | |
| Max frame time (ms) | | | |
| blink CPU usage (% total) | n/a | | n/a |
| GPU 3D engine utilization (%) | | | |
| GPU video encode utilization (%) | | | |

### Stretched 4:3 (`1280x960` scaled to panel)

| Metric | blink off | blink recording | Delta |
| --- | --- | --- | --- |
| Average FPS | | | |
| 1% low FPS | | | |
| Frame time p50 (ms) | | | |
| Frame time p95 (ms) | | | |
| Frame time p99 (ms) | | | |
| Max frame time (ms) | | | |
| blink CPU usage (% total) | n/a | | n/a |
| GPU 3D engine utilization (%) | | | |
| GPU video encode utilization (%) | | | |

### Environment

| Field | Value |
| --- | --- |
| CPU | |
| GPU | |
| NVIDIA driver version | |
| Windows version and build | |
| Display mode and refresh rate | |
| GPU scaling mode | |
| Game build | |
| blink commit | |
| PresentMon or CapFrameX version | |
| Runs per state | |

## Interpreting results

- FPS noise between identical runs of the same build is typically a few percent.
  Do not claim a win that sits inside the noise floor; increase run count or
  lengthen the run instead.
- A regression in 1% lows or p99 frame time matters more than a small drop in
  average FPS, even when the average barely moves.
- Encoder utilization rising while 3D utilization holds steady is the expected
  signature. If 3D utilization rises, something in the pipeline is doing work on
  the 3D engine that should be on NVENC or on the CPU.
- If CPU usage rises per frame, look for allocation in the hot path first, then
  for accidental blocking or a copy.

Hosted CI runners have no NVIDIA GPU, so none of this is automated. The numbers
come from a human on hardware, and that is stated in the pull request template.
