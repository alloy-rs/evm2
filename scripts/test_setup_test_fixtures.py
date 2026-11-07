import asyncio
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, Mock, patch

import setup_test_fixtures as fixtures


class PipeToTarTests(unittest.IsolatedAsyncioTestCase):
    async def test_drains_stderr_while_writing_input(self):
        stderr_drained = asyncio.Event()

        async def read_stderr():
            stderr_drained.set()
            return b""

        async def chunks(**_):
            yield b"archive data"

        # Model backpressure: tar cannot consume input until its stderr is drained.
        stdin = SimpleNamespace(
            write=Mock(), drain=stderr_drained.wait,
            close=Mock(), wait_closed=AsyncMock(),
        )
        process = SimpleNamespace(
            stdin=stdin, stderr=SimpleNamespace(read=read_stderr),
            wait=AsyncMock(return_value=0), returncode=None, kill=Mock(),
        )
        response = SimpleNamespace(aiter_bytes=chunks, headers={})
        fixture = SimpleNamespace(dest=Path("unused"), excludes=())
        with patch.object(fixtures.asyncio, "create_subprocess_exec", AsyncMock(return_value=process)):
            await asyncio.wait_for(fixtures.pipe_to_tar(response, fixture, Mock(), 0), timeout=2)
        stdin.write.assert_called_once_with(b"archive data")
        stdin.close.assert_called_once()

    async def test_stderr_failure_does_not_mask_input_failure(self):
        stderr_failed = asyncio.Event()

        async def read_stderr():
            stderr_failed.set()
            raise ValueError("stderr failed")

        async def fail_input():
            await stderr_failed.wait()
            raise OSError("input failed")

        async def chunks(**_):
            yield b"archive data"

        process = SimpleNamespace(
            stdin=SimpleNamespace(write=Mock(), drain=fail_input),
            stderr=SimpleNamespace(read=read_stderr),
            wait=AsyncMock(return_value=1), returncode=None, kill=Mock(),
        )
        response = SimpleNamespace(aiter_bytes=chunks, headers={})
        fixture = SimpleNamespace(dest=Path("unused"), excludes=())
        with patch.object(fixtures.asyncio, "create_subprocess_exec", AsyncMock(return_value=process)):
            with self.assertRaisesRegex(OSError, "input failed"):
                await asyncio.wait_for(fixtures.pipe_to_tar(response, fixture, Mock(), 0), timeout=2)
        process.kill.assert_called_once()
        process.wait.assert_awaited_once()


if __name__ == "__main__":
    unittest.main()
