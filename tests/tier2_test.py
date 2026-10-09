#!/usr/bin/env python3
"""Tier 2 — proves the yt-transcribe MCP server under a REAL MCP client.

Why a hand-written JSON-RPC script is not enough: it exercises neither keepalive, nor the
lifecycle, nor version negotiation. The real client is what Hermes uses underneath. Here:
`stdio_client` + `ClientSession` from the `mcp` package.

Usage:
    python3 tests/tier2_test.py

The video below is public, short, and has a caption track, so the run takes seconds: what is being
proved here is the MCP surface, not the recognizer.
"""
import asyncio
import json
import os
import sys
import time

from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "yt-transcribe")
VIDEO = "jNQXAC9IVRw"  # 19 s, public, with captions
WORKDIR = "/tmp/ytx-mcp-tier2"


def text_of(result) -> str:
    return result.content[0].text if result.content else ""


async def main() -> int:
    if not os.path.exists(BIN):
        print(f"FAILED: no binary at {BIN} (run cargo build --release)")
        return 1
    os.makedirs(WORKDIR, exist_ok=True)
    # A clean slate: the registry is the source of truth, and this test wants a fresh one.
    for name in os.listdir(WORKDIR):
        os.remove(os.path.join(WORKDIR, name))

    failures = []
    params = StdioServerParameters(command=BIN, args=["mcp"], env={})

    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            info = await session.initialize()
            print(f"handshake: {info.serverInfo.name} {info.serverInfo.version}")
            print(f"protocolVersion negotiated: {info.protocolVersion}")

            tools = await session.list_tools()
            names = [t.name for t in tools.tools]
            print(f"tools: {len(names)} -> {names}")
            missing = {"echo", "submit", "status", "result", "cancel"} - set(names)
            if missing:
                print(f"FAILED: missing tools: {sorted(missing)}")
                return 1

            said = text_of(await session.call_tool("echo", {"text": "phase 3 -- ção ✓"}))
            print(f"echo: {said}")
            if not said.startswith("phase 3") or "echo at" not in said:
                failures.append("echo did not prove liveness")

            # `submit` must return at once; the job runs in the background.
            t0 = time.time()
            res = await session.call_tool(
                "submit",
                {"source": f"https://www.youtube.com/watch?v={VIDEO}", "dir": WORKDIR},
            )
            took = time.time() - t0
            job = json.loads(text_of(res))
            print(f"submit -> {job} ({took:.2f} s)")
            if job.get("job") != VIDEO or job.get("state") != "running":
                failures.append("submit did not start the job")
            if took > 1.0:
                failures.append(f"submit took {took:.2f} s — it must return at once")

            # `status` reads the record on disk, so polling it is how progress is watched.
            state, status = "running", {}
            for _ in range(90):
                await asyncio.sleep(1)
                status = json.loads(
                    text_of(await session.call_tool("status", {"document": VIDEO, "dir": WORKDIR}))
                )
                state = (status.get("record") or {}).get("state")
                if state and state != "running":
                    break
            print(
                f"status -> state={state} cues={status.get('cues_recorded')} "
                f"running={status.get('running')}"
            )
            if state != "finished":
                failures.append(f"job ended in state {state!r}")
            if not status.get("cues_recorded"):
                failures.append("no cues were recorded")

            # The summary: enough to know what is there, small enough not to burn context.
            summary = text_of(await session.call_tool("result", {"document": VIDEO, "dir": WORKDIR}))
            print(f"result (summary):\n{summary}")
            s = json.loads(summary)
            if s.get("mode") != "summary" or not s.get("cues"):
                failures.append("the no-argument result is not a summary")
            if len(summary) > 1024:
                failures.append(f"the summary is {len(summary)} chars — it must stay under 1 KB")
            if "at" not in json.dumps(s.get("first", {})):
                failures.append("the first cue came without its time")

            # A window, by the clock.
            window = json.loads(
                text_of(
                    await session.call_tool(
                        "result", {"document": VIDEO, "dir": WORKDIR, "from": 2.0, "to": 12.0}
                    )
                )
            )
            print(
                f"result (window 2..12 s): {len(window.get('cues', []))} cues, "
                f"seconds={window.get('seconds')}"
            )
            if window.get("mode") != "window" or not window.get("cues"):
                failures.append("the window returned nothing")
            if any("at" not in cue for cue in window.get("cues", [])):
                failures.append("a cue in the window came without its time")

            # A search, with the neighbours that make a hit quotable.
            found = json.loads(
                text_of(
                    await session.call_tool(
                        "result", {"document": VIDEO, "dir": WORKDIR, "term": "the", "context": 1}
                    )
                )
            )
            print(
                f"result (term 'the'): {found.get('matches')} matches, "
                f"{len(found.get('cues', []))} cues with context"
            )
            if found.get("mode") != "search" or not found.get("cues"):
                failures.append("the search found nothing")
            if any("at" not in cue for cue in found.get("cues", [])):
                failures.append("a matched cue came without its time")

            # The whole text, when it is really wanted.
            whole = json.loads(
                text_of(
                    await session.call_tool("result", {"document": VIDEO, "dir": WORKDIR, "full": True})
                )
            )
            print(f"result (full): {whole.get('characters')} chars")
            if whole.get("mode") != "full" or not whole.get("text"):
                failures.append("full returned no text")

            # Cancelling something that has already finished must say so, not pretend.
            cancelled = json.loads(
                text_of(await session.call_tool("cancel", {"document": VIDEO}))
            )
            print(f"cancel (not running) -> {cancelled}")
            if cancelled.get("state") != "not running":
                failures.append("cancel on a finished job did not say so")

    if failures:
        print()
        for f in failures:
            print(f"FAILED: {f}")
        return 1

    print("\nOK — the Rust server answers a real MCP client (Tier 2).")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
