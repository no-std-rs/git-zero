"""Pinned Git and explicit codec/delta policies for reproducible comparisons."""
from dataclasses import dataclass
from pathlib import Path


REFERENCE_GIT_VERSION = Path(__file__).with_name("git-version.txt").read_text().strip()


@dataclass(frozen=True)
class GitConfig:
    profile: str
    compression: int | None
    delta_window: int | None
    delta_depth: int | None

    def overrides(self):
        values = {"core.compression": self.compression, "core.looseCompression": self.compression,
                  "pack.compression": self.compression, "pack.window": self.delta_window,
                  "pack.depth": self.delta_depth}
        return {key: value for key, value in values.items() if value is not None}

    def algorithms(self):
        deltas = self.delta_window != 0 and self.delta_depth != 0
        equivalent = self.compression == 0 and not deltas
        return {"profile": self.profile, "equivalent_server_features": equivalent,
                "git": {"compression": "stored-deflate" if self.compression == 0 else "deflate",
                        "level": self.compression, "delta_window": self.delta_window,
                        "delta_depth": self.delta_depth, "deltas": deltas},
                "zero": {"compression": "stored-deflate", "level": 0, "deltas": False},
                "git_config_overrides": self.overrides()}


def configuration(profile="matched", compression=None, delta_window=None, delta_depth=None):
    if profile not in ("matched", "upstream-defaults"):
        raise ValueError("unknown Git profile")
    supplied = (compression, delta_window, delta_depth)
    if profile == "upstream-defaults":
        if any(value is not None for value in supplied):
            raise ValueError("upstream-defaults cannot be combined with compression/delta overrides")
        return GitConfig(profile, None, None, None)
    compression, delta_window, delta_depth = (0 if value is None else value for value in supplied)
    if not 0 <= compression <= 9 or delta_window < 0 or not 0 <= delta_depth <= 4095:
        raise ValueError("compression must be 0..9, delta window nonnegative and depth 0..4095")
    profile = "matched" if (compression, delta_window, delta_depth) == (0, 0, 0) else "custom"
    return GitConfig(profile, compression, delta_window, delta_depth)


def coerce(value):
    """Keep existing fixture helpers accepting their numeric compression option."""
    return value if isinstance(value, GitConfig) else configuration(compression=value)


def version_pin(actual, allow_unpinned=False):
    expected = f"git version {REFERENCE_GIT_VERSION}"
    matches = actual == expected
    if not matches and not allow_unpinned:
        raise ValueError(f"benchmark requires {expected}; got {actual}. Select --git or explicitly use --allow-unpinned-git for a smoke test")
    return {"expected": expected, "actual": actual, "matches": matches, "required": not allow_unpinned}
