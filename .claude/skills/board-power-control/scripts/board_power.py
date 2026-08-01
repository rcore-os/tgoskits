#!/usr/bin/env python3
"""Safely control the smart plug powering the OrangePi-5-Plus board."""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import re
import sys
import time
import tomllib
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Protocol

DEFAULT_CONFIG_PATH = Path(".board-power.toml")
DEFAULT_TOKEN_ENV = "TGOS_BOARD_POWER_TOKEN"
TOKEN_PATTERN = re.compile(r"^[0-9a-fA-F]{32}$")


@dataclass(frozen=True)
class PlugConfig:
    """Validated connection and power-cycle settings."""

    name: str
    ip: str
    model: str
    token: str
    off_seconds: float
    timeout_seconds: int


@dataclass(frozen=True)
class PowerStatus:
    """The board power state and the plug's relevant health readings."""

    name: str
    power_on: bool
    electric_power_watts: float | int | None
    temperature_celsius: float | int | None
    fault_code: int | None


class MiotStatus(Protocol):
    """Subset of python-miio status used by this tool."""

    def property_dict(self) -> Mapping[str, Any]: ...


class MiotDevice(Protocol):
    """Subset of python-miio device operations used by this tool."""

    def status(self) -> MiotStatus: ...

    def set_property_by(
        self,
        siid: int,
        piid: int,
        value: bool,
        *,
        name: str,
    ) -> Any: ...


def main(argv: list[str] | None = None) -> int:
    """Load configuration, perform one action, and print a safe result."""

    try:
        args = parse_args(argv)
        config = load_config(args.config, os.environ)
        ensure_action_is_authorized(args.action, args.yes)
        status = execute_action(
            config,
            args.action,
            off_seconds=args.off_seconds,
        )
        print_status(args.action, status, args.json)
        return 0
    except (ConfigError, PowerControlError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("ERROR: interrupted", file=sys.stderr)
        return 130


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Control the smart plug powering OrangePi-5-Plus",
    )
    parser.add_argument("action", choices=("status", "on", "off", "cycle"))
    parser.add_argument(
        "--config",
        type=Path,
        default=Path(os.environ.get("TGOS_BOARD_POWER_CONFIG", DEFAULT_CONFIG_PATH)),
        help="configuration file (default: .board-power.toml)",
    )
    parser.add_argument(
        "--off-seconds",
        type=float,
        help="override the cold-cycle off interval",
    )
    parser.add_argument(
        "--yes",
        action="store_true",
        help="confirm disruptive off or cycle action",
    )
    parser.add_argument("--json", action="store_true", help="print JSON result")
    return parser.parse_args(argv)


def load_config(path: Path, environment: Mapping[str, str]) -> PlugConfig:
    """Load and validate the ignored local smart-plug configuration."""

    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ConfigError(
            f"missing {path}; copy the board-power-control config example first",
        ) from error
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ConfigError(f"cannot read {path}: {error}") from error

    section = document.get("board_power")
    if not isinstance(section, dict):
        raise ConfigError(f"{path} must contain a [board_power] table")

    token_env = string_value(section, "token_env", DEFAULT_TOKEN_ENV)
    token = environment.get(token_env) or string_value(section, "token", "")
    config = PlugConfig(
        name=string_value(section, "name", "orangepi-5-plus-rk3588"),
        ip=required_string(section, "ip"),
        model=string_value(section, "model", "cuco.plug.v3"),
        token=token,
        off_seconds=number_value(section, "off_seconds", 8.0),
        timeout_seconds=int(number_value(section, "timeout_seconds", 5)),
    )
    validate_config(config, token_env)
    return config


def ensure_action_is_authorized(action: str, confirmed: bool) -> None:
    """Reject disruptive actions that were not explicitly confirmed."""

    if action in {"off", "cycle"} and not confirmed:
        raise PowerControlError(f"{action} requires --yes because it cuts board power")


def execute_action(
    config: PlugConfig,
    action: str,
    *,
    off_seconds: float | None = None,
    device_factory: Callable[[PlugConfig], MiotDevice] | None = None,
    sleep: Callable[[float], None] = time.sleep,
) -> PowerStatus:
    """Execute a power action and return the observed final state."""

    factory = device_factory or create_device
    device = factory(config)
    if action == "status":
        return read_status(config, device)
    if action == "on":
        return transition_power(config, device, True, sleep)
    if action == "off":
        return transition_power(config, device, False, sleep)
    if action == "cycle":
        interval = config.off_seconds if off_seconds is None else off_seconds
        validate_off_seconds(interval)
        return cycle_power(config, device, interval, sleep)
    raise PowerControlError(f"unsupported action: {action}")


def create_device(config: PlugConfig) -> MiotDevice:
    try:
        from miio.integrations.genericmiot.genericmiot import GenericMiot
    except ImportError as error:
        raise ConfigError(
            "python-miio is missing; install board-power-control/requirements.txt",
        ) from error

    return GenericMiot(
        config.ip,
        config.token,
        model=config.model,
        timeout=config.timeout_seconds,
    )


def cycle_power(
    config: PlugConfig,
    device: MiotDevice,
    off_seconds: float,
    sleep: Callable[[float], None],
) -> PowerStatus:
    powered_off = False
    try:
        transition_power(config, device, False, sleep)
        powered_off = True
        sleep(off_seconds)
    except BaseException:
        if powered_off:
            transition_power(config, device, True, sleep)
        raise
    return transition_power(config, device, True, sleep)


def transition_power(
    config: PlugConfig,
    device: MiotDevice,
    power_on: bool,
    sleep: Callable[[float], None],
) -> PowerStatus:
    properties = read_properties(config, device)
    current = status_from_properties(config, properties)
    if power_on and current.fault_code not in {None, 0}:
        raise PowerControlError(
            f"refusing to power on {config.name}: plug fault code {current.fault_code}",
        )
    switch_property = properties.get("switch:on")
    service = getattr(switch_property, "service", None)
    siid = getattr(service, "siid", None)
    piid = getattr(switch_property, "piid", None)
    if not isinstance(siid, int) or not isinstance(piid, int):
        raise PowerControlError("MIoT switch:on property has no writable siid/piid")
    try:
        response = device.set_property_by(
            siid,
            piid,
            power_on,
            name="switch:on",
        )
    except Exception as error:
        requested_state = "on" if power_on else "off"
        raise PowerControlError(
            f"cannot switch {config.name} {requested_state}: {error}",
        ) from error
    validate_set_response(response)
    return wait_for_power_state(config, device, power_on, sleep)


def wait_for_power_state(
    config: PlugConfig,
    device: MiotDevice,
    expected: bool,
    sleep: Callable[[float], None],
) -> PowerStatus:
    observed: PowerStatus | None = None
    for attempt in range(4):
        observed = read_status(config, device)
        if observed.power_on is expected:
            return observed
        if attempt < 3:
            sleep(0.5)
    state = "on" if observed and observed.power_on else "off"
    expected_state = "on" if expected else "off"
    raise PowerControlError(
        f"plug remained {state} after requesting {expected_state}",
    )


def read_status(config: PlugConfig, device: MiotDevice) -> PowerStatus:
    properties = read_properties(config, device)
    return status_from_properties(config, properties)


def read_properties(
    config: PlugConfig,
    device: MiotDevice,
) -> Mapping[str, Any]:
    try:
        return device.status().property_dict()
    except Exception as error:
        raise PowerControlError(
            f"cannot read {config.name} plug at {config.ip}: {error}",
        ) from error


def status_from_properties(
    config: PlugConfig,
    properties: Mapping[str, Any],
) -> PowerStatus:
    return PowerStatus(
        name=config.name,
        power_on=bool(property_value(properties, "switch:on", required=True)),
        electric_power_watts=property_value(
            properties,
            "power-consumption:electric-power",
        ),
        temperature_celsius=property_value(
            properties,
            "on-off-count:temperature",
        ),
        fault_code=property_value(properties, "switch:fault"),
    )


def validate_set_response(response: Any) -> None:
    results = response if isinstance(response, list) else [response]
    for result in results:
        if isinstance(result, dict) and result.get("code", 0) != 0:
            raise PowerControlError(f"MIoT rejected power transition: {result}")


def property_value(
    properties: Mapping[str, Any],
    key: str,
    *,
    required: bool = False,
) -> Any:
    property_state = properties.get(key)
    if property_state is None:
        if required:
            raise PowerControlError(f"MIoT property is missing: {key}")
        return None
    return getattr(property_state, "value", property_state)


def validate_config(config: PlugConfig, token_env: str) -> None:
    try:
        ipaddress.ip_address(config.ip)
    except ValueError as error:
        raise ConfigError(f"invalid smart-plug IP: {config.ip}") from error
    if not config.name.strip():
        raise ConfigError("board_power.name cannot be empty")
    if not config.model.strip():
        raise ConfigError("board_power.model cannot be empty")
    if not TOKEN_PATTERN.fullmatch(config.token):
        raise ConfigError(
            f"set {token_env} to the plug's 32-character hexadecimal token",
        )
    validate_off_seconds(config.off_seconds)
    if not 1 <= config.timeout_seconds <= 30:
        raise ConfigError("timeout_seconds must be between 1 and 30")


def validate_off_seconds(seconds: float) -> None:
    if not 1 <= seconds <= 120:
        raise ConfigError("off_seconds must be between 1 and 120")


def required_string(section: Mapping[str, Any], key: str) -> str:
    value = section.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ConfigError(f"board_power.{key} must be a non-empty string")
    return value.strip()


def string_value(section: Mapping[str, Any], key: str, default: str) -> str:
    value = section.get(key, default)
    if not isinstance(value, str):
        raise ConfigError(f"board_power.{key} must be a string")
    return value.strip()


def number_value(section: Mapping[str, Any], key: str, default: float) -> float:
    value = section.get(key, default)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ConfigError(f"board_power.{key} must be a number")
    return float(value)


def print_status(action: str, status: PowerStatus, as_json: bool) -> None:
    result = {"action": action, **asdict(status)}
    if as_json:
        print(json.dumps(result, separators=(",", ":"), sort_keys=True))
        return
    state = "on" if status.power_on else "off"
    print(
        "RESULT "
        f"action={action} board={status.name} power={state} "
        f"watts={status.electric_power_watts} "
        f"temperature_celsius={status.temperature_celsius} "
        f"fault_code={status.fault_code}",
    )


class ConfigError(Exception):
    """Raised when local board-power configuration is missing or invalid."""


class PowerControlError(Exception):
    """Raised when a requested plug operation fails or cannot be verified."""


if __name__ == "__main__":
    raise SystemExit(main())
