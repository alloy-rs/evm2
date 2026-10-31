"""Shared utilities for repository scripts."""

import os


def repo_root() -> str:
    """Return the repository root."""
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def cargo_env() -> dict[str, str]:
    """Return the default cargo environment for scripts."""
    return {**os.environ, "NO_COLOR": "1"}
