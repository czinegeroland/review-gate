"""Unified-diff parsing and chunking."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from .errors import DiffError

_DIFF_HEADER = re.compile(r"^diff --git a/(?P<a>.+?) b/(?P<b>.+)$")
_OLD_FILE = re.compile(r"^--- (?:a/)?(?P<path>.+)$")
_NEW_FILE = re.compile(r"^\+\+\+ (?:b/)?(?P<path>.+)$")
_HUNK = re.compile(r"^@@ ")


@dataclass
class FileDiff:
    """The patch for a single file."""

    path: str
    status: str = "modified"
    lines: list[str] = field(default_factory=list)
    binary: bool = False

    @property
    def text(self) -> str:
        return "\n".join(self.lines)

    @property
    def added(self) -> int:
        return sum(1 for line in self.lines if line.startswith("+") and not line.startswith("+++"))

    @property
    def removed(self) -> int:
        return sum(1 for line in self.lines if line.startswith("-") and not line.startswith("---"))

    def hunks(self) -> list[str]:
        """Split the patch body into hunks, each prefixed with the file header."""
        header: list[str] = []
        chunks: list[list[str]] = []
        for line in self.lines:
            if _HUNK.match(line):
                chunks.append([line])
            elif chunks:
                chunks[-1].append(line)
            else:
                header.append(line)
        if not chunks:
            return [self.text]
        prefix = "\n".join(header)
        return [(prefix + "\n" + "\n".join(chunk)).strip("\n") for chunk in chunks]


@dataclass
class Chunk:
    """A slice of the diff small enough to send as model state in one request."""

    files: list[str]
    text: str


def parse_diff(diff_text: str) -> list[FileDiff]:
    """Parse a unified diff (``git diff``/GitHub ``.diff``) into per-file patches."""
    files: list[FileDiff] = []
    current: FileDiff | None = None

    for line in diff_text.splitlines():
        header = _DIFF_HEADER.match(line)
        if header:
            current = FileDiff(path=header.group("b"), lines=[line])
            files.append(current)
            continue
        if current is None:
            continue

        current.lines.append(line)
        if line.startswith("new file mode"):
            current.status = "added"
        elif line.startswith("deleted file mode"):
            current.status = "deleted"
        elif line.startswith("rename to "):
            current.status = "renamed"
            current.path = line[len("rename to ") :].strip()
        elif line.startswith("Binary files ") or line.startswith("GIT binary patch"):
            current.binary = True
        elif line.startswith("+++ "):
            match = _NEW_FILE.match(line)
            if match and match.group("path") != "/dev/null":
                current.path = match.group("path")
        elif line.startswith("--- ") and current.status == "deleted":
            match = _OLD_FILE.match(line)
            if match and match.group("path") != "/dev/null":
                current.path = match.group("path")

    return [file for file in files if file.path]


def read_diff(diff_file: str | None, base: str | None, head: str | None, repo_dir: Path) -> str:
    """Read a diff from a file, stdin (``-``) or ``git diff base...head``."""
    if diff_file is not None:
        if diff_file == "-":
            import sys

            return sys.stdin.read()
        path = Path(diff_file)
        if not path.is_file():
            raise DiffError(f"diff file not found: {path}")
        return path.read_text(encoding="utf-8", errors="replace")

    if base is None:
        raise DiffError("provide --diff-file or --base (optionally with --head)")

    revision = f"{base}...{head}" if head else base
    command = ["git", "diff", "--no-color", "--find-renames", revision]
    try:
        completed = subprocess.run(
            command, cwd=repo_dir, capture_output=True, text=True, check=False
        )
    except OSError as exc:
        raise DiffError(f"cannot run git: {exc}") from exc
    if completed.returncode != 0:
        raise DiffError(f"`{' '.join(command)}` failed: {completed.stderr.strip()}")
    return completed.stdout


def chunk_files(files: list[FileDiff], max_chars: int) -> list[Chunk]:
    """Pack file patches into chunks of at most ``max_chars`` characters.

    A single file larger than the budget is split by hunk; a single hunk that is
    still too large is truncated, with a marker so the model knows.
    """
    chunks: list[Chunk] = []
    current_parts: list[str] = []
    current_files: list[str] = []
    current_len = 0

    def flush() -> None:
        nonlocal current_parts, current_files, current_len
        if current_parts:
            chunks.append(Chunk(files=current_files, text="\n".join(current_parts)))
        current_parts, current_files, current_len = [], [], 0

    for file in files:
        text = file.text
        pieces = [text] if len(text) <= max_chars else _split_oversized(file, max_chars)

        for piece in pieces:
            if current_len and current_len + len(piece) > max_chars:
                flush()
            current_parts.append(piece)
            current_len += len(piece) + 1
            if file.path not in current_files:
                current_files.append(file.path)

    flush()
    return chunks


def _split_oversized(file: FileDiff, max_chars: int) -> list[str]:
    pieces: list[str] = []
    for hunk in file.hunks():
        if len(hunk) <= max_chars:
            pieces.append(hunk)
        else:
            marker = f"\n[review-gate: hunk truncated, {len(hunk) - max_chars} characters omitted]"
            pieces.append(hunk[: max_chars - len(marker)] + marker)
    return pieces
