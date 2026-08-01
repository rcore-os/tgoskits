---
name: board-power-control
description: Control the OrangePi-5-Plus RK3588 board's Xiaomi cuco.plug.v3 smart plug. Use this skill when the physical board needs power status, power-on, power-off, or a cold power cycle, especially when board connect, serial, or SSH cannot recover the board.
---

# Board Power Control

## Purpose

Use the repository tool to control the Xiaomi `cuco.plug.v3` that supplies the
OrangePi-5-Plus RK3588 board. The tool reads the token from the ignored local
`.board-power.toml` file or from an environment variable and never prints it.

## Setup

Run from the repository root. Python 3.11 or newer is required.

```bash
python -m pip install -r .claude/skills/board-power-control/requirements.txt
```

Copy `references/config.example.toml` to `.board-power.toml`, then provide the
token either in that ignored local file or through `TGOS_BOARD_POWER_TOKEN`.
Never add a real token to a tracked file, issue, log, or chat response.

## Commands

Always query state before changing power:

```bash
python .claude/skills/board-power-control/scripts/board_power.py status
```

Power on is safe to retry and does not require confirmation:

```bash
python .claude/skills/board-power-control/scripts/board_power.py on
```

Power off and cold-cycle are disruptive and require `--yes`:

```bash
python .claude/skills/board-power-control/scripts/board_power.py off --yes
python .claude/skills/board-power-control/scripts/board_power.py cycle --yes
```

Use `--off-seconds <seconds>` to override the configured cold-cycle delay. The
tool verifies the observed plug state after every transition and returns a
nonzero exit status if the plug does not reach the requested state.

For machine-readable output:

```bash
python .claude/skills/board-power-control/scripts/board_power.py status --json
```

## Safety Rules

- Do not power off or cycle while a board test, filesystem write, image flash,
  package update, or `board connect` deployment is active.
- Release any board-service lease before a cold cycle unless the recovery
  procedure explicitly requires holding it.
- Prefer `on` when the current state is unknown. Use `cycle` only for a genuine
  cold-boot recovery or when the user explicitly requests it.
- After a cold cycle, wait for serial boot output before starting SSH or a board
  workload.
- If the plug reports overload or over-temperature, leave it off and report the
  fault instead of repeatedly powering it on.

## Expected Evidence

Keep the final `RESULT` line. It includes the action, resulting power state,
power reading, plug temperature, and fault code without exposing the token.
