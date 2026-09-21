from __future__ import annotations

from pathlib import Path

import pytest

from review_gate.diff import FileDiff, chunk_files, parse_diff, read_diff
from review_gate.errors import DiffError


def test_parses_files_and_status(sample_diff: str) -> None:
    files = parse_diff(sample_diff)
    assert [f.path for f in files] == [
        "services/orders/handler.py",
        "infra/stack.ts",
        "assets/logo.png",
    ]
    assert files[0].added == 1
    assert files[0].removed == 2
    assert files[2].binary is True
    assert all(f.status == "modified" for f in files)


def test_parses_added_deleted_renamed() -> None:
    diff = """diff --git a/new.py b/new.py
new file mode 100644
--- /dev/null
+++ b/new.py
@@ -0,0 +1 @@
+print("hi")
diff --git a/gone.py b/gone.py
deleted file mode 100644
--- a/gone.py
+++ /dev/null
@@ -1 +0,0 @@
-print("bye")
diff --git a/old.py b/moved.py
similarity index 100%
rename from old.py
rename to moved.py
"""
    files = parse_diff(diff)
    assert [(f.path, f.status) for f in files] == [
        ("new.py", "added"),
        ("gone.py", "deleted"),
        ("moved.py", "renamed"),
    ]


def test_parse_ignores_noise() -> None:
    assert parse_diff("not a diff at all\n") == []


def test_chunking_packs_and_splits() -> None:
    small = parse_diff(
        "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1 +1 @@\n-a\n+b\n"
    )[0]
    other = FileDiff(path="b.py", lines=["diff --git a/b.py b/b.py", "@@ -1 +1 @@", "-x", "+y"])

    packed = chunk_files([small, other], max_chars=10_000)
    assert len(packed) == 1
    assert packed[0].files == ["a.py", "b.py"]

    split = chunk_files([small, other], max_chars=len(small.text) + 1)
    assert [chunk.files for chunk in split] == [["a.py"], ["b.py"]]


def test_oversized_file_is_split_by_hunk_and_truncated() -> None:
    body = "\n".join(f"+line {i}" for i in range(500))
    big = FileDiff(
        path="big.py",
        lines=["diff --git a/big.py b/big.py", "@@ -1 +1 @@", body, "@@ -2 +2 @@", "+tail"],
    )
    chunks = chunk_files([big], max_chars=600)
    assert len(chunks) >= 2
    assert any("truncated" in chunk.text for chunk in chunks)
    assert all(len(chunk.text) <= 600 for chunk in chunks)


def test_read_diff_from_file_and_errors(tmp_path: Path) -> None:
    path = tmp_path / "x.diff"
    path.write_text("diff --git a/a b/a\n")
    assert read_diff(str(path), None, None, tmp_path).startswith("diff --git")
    with pytest.raises(DiffError, match="not found"):
        read_diff(str(tmp_path / "nope.diff"), None, None, tmp_path)
    with pytest.raises(DiffError, match="--diff-file or --base"):
        read_diff(None, None, None, tmp_path)
