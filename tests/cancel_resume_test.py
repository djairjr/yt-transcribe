#!/usr/bin/env python3
"""Tier 2 for the task side: cancel, and resume — under a REAL MCP client.

Three things are proved here, and none of them can be proved from a hand-written JSON-RPC script:

  * the state **survives the process**: the server is closed and a new one is spawned, and `status`
    still answers with the same numbers, because the record and the registry are on disk;
  * `cancel` **preserves** what was finished: the run stops between ASR blocks, the cue registry is
    not rolled back, and the record says `cancelled`;
  * resuming **costs nothing**: submitting the same video again starts at the block after the last
    one recorded instead of at the first.

The file is passed as the first argument (a path to media with no caption track, long enough to
hold several 240-second blocks). Default: /tmp/theparty.avi.

Usage:
    python3 tests/cancel_resume_test.py [/path/to/media]
"""
import asyncio
import json
import os
import shutil
import sys

from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "yt-transcribe")
WORKDIR = "/tmp/ytx-mcp-cancel"


def text_of(result) -> str:
    return result.content[0].text if result.content else ""


def spawn():
    return StdioServerParameters(command=BIN, args=["mcp"], env={})


async def status_of(session, job: str) -> dict:
    return json.loads(text_of(await session.call_tool("status", {"document": job, "dir": WORKDIR})))


async def main() -> int:
    source = sys.argv[1] if len(sys.argv) > 1 else "/tmp/theparty.avi"
    if not os.path.isfile(source):
        print(f"FAILED: {source} is not a file")
        return 1
    if not os.path.exists(BIN):
        print(f"FAILED: no binary at {BIN}")
        return 1

    shutil.rmtree(WORKDIR, ignore_errors=True)
    os.makedirs(WORKDIR, exist_ok=True)
    job = os.path.splitext(os.path.basename(source))[0]
    failures = []

    # --- first run: let it get a couple of blocks in, then cancel -------------------
    async with stdio_client(spawn()) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            started = json.loads(
                text_of(await session.call_tool("submit", {"source": source, "dir": WORKDIR}))
            )
            print(f"submit -> {started}")
            if started.get("resumed_from_chunk") != 0:
                failures.append("a fresh job did not start at block 0")

            # Wait for at least one whole block, or give up after 5 minutes.
            status = {}
            for _ in range(150):
                await asyncio.sleep(2)
                status = await status_of(session, job)
                done = (status.get("record") or {}).get("chunks_done", 0)
                if done >= 1:
                    break
            done = (status.get("record") or {}).get("chunks_done", 0)
            print(f"after waiting: blocks done = {done}, cues = {status.get('cues_recorded')}")
            if done < 1:
                print("FAILED: no block finished in 5 minutes -- cannot test cancel")
                return 1

            cancelled = json.loads(text_of(await session.call_tool("cancel", {"document": job})))
            print(f"cancel -> {cancelled}")

            # The block in flight has to finish first, so wait for the state to settle.
            state = "running"
            for _ in range(90):
                await asyncio.sleep(2)
                status = await status_of(session, job)
                state = (status.get("record") or {}).get("state")
                if state and state != "running":
                    break
            blocks_kept = (status.get("record") or {}).get("chunks_done", 0)
            cues_kept = status.get("cues_recorded", 0)
            print(f"after cancel: state={state} blocks_kept={blocks_kept} cues_kept={cues_kept}")
            if state != "cancelled":
                failures.append(f"state after cancel is {state!r}, not 'cancelled'")
            if blocks_kept < 1 or cues_kept < 1:
                failures.append("cancel threw away the finished blocks")

    # --- the process is gone; a new one must read the same state -------------------
    async with stdio_client(spawn()) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            again = await status_of(session, job)
            blocks_again = (again.get("record") or {}).get("chunks_done", 0)
            cues_again = again.get("cues_recorded", 0)
            print(
                f"new process: state={(again.get('record') or {}).get('state')} "
                f"blocks={blocks_again} cues={cues_again}"
            )
            if blocks_again != blocks_kept or cues_again != cues_kept:
                failures.append("the state did not survive the process")

            # --- resuming must start after the last recorded block ------------------
            resumed = json.loads(
                text_of(await session.call_tool("submit", {"source": source, "dir": WORKDIR}))
            )
            print(f"resubmit -> {resumed}")
            if resumed.get("resumed_from_chunk") != blocks_kept:
                failures.append(
                    f"resume started at block {resumed.get('resumed_from_chunk')}, "
                    f"expected {blocks_kept}"
                )

            # Stop it again: this run would take the better part of an hour.
            await asyncio.sleep(2)
            print(f"cancel again -> {text_of(await session.call_tool('cancel', {'document': job}))}")

    if failures:
        print()
        for f in failures:
            print(f"FAILED: {f}")
        return 1

    print("\nOK — cancel preserves, the state survives the process, and resuming skips what is done.")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
