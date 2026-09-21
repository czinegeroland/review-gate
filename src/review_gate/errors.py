"""Exception types used across Review Gate."""

from __future__ import annotations


class ReviewGateError(Exception):
    """Base class for expected, user-facing failures."""


class ConfigError(ReviewGateError):
    """The rule configuration is missing or invalid."""


class DiffError(ReviewGateError):
    """The diff could not be read or parsed."""


class ModelError(ReviewGateError):
    """The classification backend could not answer."""
