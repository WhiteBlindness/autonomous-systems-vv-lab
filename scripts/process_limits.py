"""Limites de execução e artefactos para Linux e Windows."""

from __future__ import annotations

import math
import subprocess
import threading
import time
from pathlib import Path


class ProcessLimitError(RuntimeError):
    """Um processo excedeu o limite de tempo ou de saída definido."""


class CampaignArtifactBudget:
    """Conta ficheiros da campanha e aplica o limite de bytes retidos."""

    def __init__(self, root: Path, max_bytes: int) -> None:
        if type(max_bytes) is not int or max_bytes < 1:
            raise ProcessLimitError("campaign has an invalid artifact byte limit")
        self.root = root.resolve()
        self.max_bytes = max_bytes
        self.total_bytes = tree_size_bytes(self.root)
        self._scope_sizes: dict[Path, int] = {}
        self._check_limit()

    def refresh(self, scope: Path) -> int:
        resolved = scope.resolve()
        try:
            resolved.relative_to(self.root)
        except ValueError as error:
            raise ProcessLimitError("artifact accounting scope is outside its campaign") from error
        current_size = tree_size_bytes(resolved) if resolved.exists() else 0
        previous_size = self._scope_sizes.get(resolved, 0)
        self.total_bytes += current_size - previous_size
        self._scope_sizes[resolved] = current_size
        self._check_limit()
        return self.total_bytes

    def remaining(self, scope: Path) -> int:
        self.refresh(scope)
        return max(0, self.max_bytes - self.total_bytes)

    def _check_limit(self) -> None:
        if self.total_bytes > self.max_bytes:
            raise ProcessLimitError(
                f"campaign artifacts used {self.total_bytes} bytes, above the {self.max_bytes}-byte limit"
            )


def run_bounded_process(
    command: list[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    max_output_bytes: int,
    description: str,
) -> subprocess.CompletedProcess[str]:
    """Executa um processo com limites de tempo e de saída combinada."""
    if (
        isinstance(timeout_seconds, bool)
        or not isinstance(timeout_seconds, (int, float))
        or not math.isfinite(timeout_seconds)
        or timeout_seconds <= 0
    ):
        raise ProcessLimitError(f"{description} exceeded the time budget")
    if type(max_output_bytes) is not int or max_output_bytes < 1:
        raise ProcessLimitError(f"{description} has an invalid output byte limit")
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise ProcessLimitError(f"{description} could not start: {error}") from error

    chunks = {"stdout": bytearray(), "stderr": bytearray()}
    output_size = 0
    lock = threading.Lock()
    output_limit_hit = threading.Event()

    def drain(stream_name: str, stream) -> None:
        nonlocal output_size
        while True:
            chunk = stream.read(8192)
            if not chunk:
                return
            with lock:
                if output_size + len(chunk) > max_output_bytes:
                    output_limit_hit.set()
                    return
                chunks[stream_name].extend(chunk)
                output_size += len(chunk)

    assert process.stdout is not None
    assert process.stderr is not None
    readers = [
        threading.Thread(target=drain, args=("stdout", process.stdout), daemon=True),
        threading.Thread(target=drain, args=("stderr", process.stderr), daemon=True),
    ]
    for reader in readers:
        reader.start()

    deadline = time.monotonic() + timeout_seconds
    timed_out = False
    while process.poll() is None:
        if output_limit_hit.is_set():
            process.kill()
            break
        if time.monotonic() >= deadline:
            timed_out = True
            process.kill()
            break
        time.sleep(0.01)
    return_code = process.wait()
    for reader in readers:
        reader.join(timeout=1)
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            stream.close()

    if output_limit_hit.is_set():
        raise ProcessLimitError(
            f"{description} exceeded the {max_output_bytes}-byte output limit"
        )
    if timed_out:
        raise ProcessLimitError(f"{description} exceeded the {timeout_seconds:g}-second timeout")

    return subprocess.CompletedProcess(
        command,
        return_code,
        stdout=chunks["stdout"].decode("utf-8", errors="replace"),
        stderr=chunks["stderr"].decode("utf-8", errors="replace"),
    )


def tree_size_bytes(root: Path) -> int:
    """Conta ficheiros regulares sem seguir ligações simbólicas."""
    total = 0
    for path in root.rglob("*"):
        try:
            metadata = path.lstat()
        except OSError:
            continue
        if path.is_symlink():
            continue
        if path.is_file():
            total += metadata.st_size
    return total


def ensure_tree_size(root: Path, max_bytes: int, description: str) -> int:
    total = tree_size_bytes(root)
    if total > max_bytes:
        raise ProcessLimitError(
            f"{description} used {total} bytes, above the {max_bytes}-byte limit"
        )
    return total
