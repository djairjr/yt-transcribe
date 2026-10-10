#!/usr/bin/env python3
"""Tier 2 for the command line: Ctrl-C is a stop, not a kill.

Everything here needs a REAL process and a REAL signal -- no unit test can prove that the handler is
installed, that the block in flight survives, or that the second interrupt leaves at once. It is
also the test that made the exit code `1` reachable from the CLI for the first time.

The file is passed as the first argument (media long enough to hold several 240-second blocks).
Default: /tmp/theparty.avi.

What is proved:
  * one SIGINT mid-transcription -> exit 1, the record says `cancelled`, the count is honest, and the
    block in flight finished and was recorded whole (no half-written line);
  * a second SIGINT -> exit 2, at once, losing nothing already finished;
  * re-running the same command -> the recorded prefix is untouched (byte for byte) and the run
    finishes clean with exit 0.

Usage:
    python3 tests/cli_signal_test.py [/path/to/media]
"""
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "yt-transcribe")
WORKDIR = "/tmp/ytx-cli-signal"

SOURCE = ""   # the path given to the binary
STEM = ""     # the file stem: the name the binary gives its files


def read_record(dirpath: str) -> dict:
    path = os.path.join(dirpath, STEM + ".task.json")
    if not os.path.isfile(path):
        return {}
    with open(path) as fh:
        return json.load(fh)


def read_registry(dirpath: str):
    """The lines as they are on disk, plus how many of them parse."""
    path = os.path.join(dirpath, STEM + ".cues.jsonl")
    if not os.path.isfile(path):
        return [], 0, 0
    with open(path, "rb") as fh:
        raw = fh.read()
    lines, valid = [], 0
    for line in raw.decode("utf-8", "replace").splitlines():
        if not line.strip():
            continue
        lines.append(line)
        try:
            json.loads(line)
            valid += 1
        except Exception:
            pass
    return lines, valid, len(lines) - valid


def spawn(dirpath: str) -> subprocess.Popen:
    return subprocess.Popen(
        [BIN, SOURCE, "-o", dirpath],
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def wait_for(fn, timeout: float, poll: float = 0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(poll)
    return None


def main() -> int:
    global SOURCE, STEM
    source = sys.argv[1] if len(sys.argv) > 1 else "/tmp/theparty.avi"
    if not os.path.isfile(source):
        print(f"FAILED: {source} is not a file")
        return 1
    if not os.path.exists(BIN):
        print(f"FAILED: no binary at {BIN}")
        return 1
    SOURCE = os.path.abspath(source)
    STEM = os.path.splitext(os.path.basename(SOURCE))[0]

    shutil.rmtree(WORKDIR, ignore_errors=True)
    os.makedirs(WORKDIR, exist_ok=True)
    failures = []

    # --- one SIGINT: the stop is honoured, between blocks ---------------------------
    stopped = os.path.join(WORKDIR, "stopped")
    os.makedirs(stopped, exist_ok=True)
    proc = spawn(stopped)
    first_block = wait_for(
        lambda: read_record(stopped).get("chunks_done") or None, timeout=300
    )
    if not first_block:
        print("FAILED: no block finished in 5 minutes -- cannot test the stop")
        return 1
    time.sleep(3)  # make sure the signal lands inside the next block
    proc.send_signal(signal.SIGINT)
    try:
        code = proc.wait(timeout=300)
    except subprocess.TimeoutExpired:
        proc.kill()
        print("FAILED: the process did not stop after Ctrl-C")
        return 1

    record = read_record(stopped)
    lines, valid, torn = read_registry(stopped)
    print(
        f"one SIGINT -> exit={code} state={record.get('state')!r} "
        f"blocks={record.get('chunks_done')} cues={record.get('cues')} lines={valid} torn={torn}"
    )
    if code != 1:
        failures.append(f"exit after one SIGINT is {code}, expected 1")
    if record.get("state") != "cancelled":
        failures.append(f"record state is {record.get('state')!r}, expected 'cancelled'")
    if (record.get("chunks_done") or 0) < 1:
        failures.append("the record says no block finished")
    if record.get("cues") != valid or valid < 1:
        failures.append(f"the count is not honest: record says {record.get('cues')}, disk has {valid}")
    if torn:
        failures.append(f"the registry has {torn} unparsable line(s)")
    if record.get("finished_at") is None:
        failures.append("the record was left open (finished_at is null)")
    for derived in (".srt", ".txt"):
        if not os.path.isfile(os.path.join(stopped, STEM + derived)):
            failures.append(f"{derived} was not written for the finished blocks")

    # --- a second SIGINT: refuse to wait -------------------------------------------
    impatient = os.path.join(WORKDIR, "impatient")
    os.makedirs(impatient, exist_ok=True)
    proc = spawn(impatient)
    wait_for(lambda: read_record(impatient).get("chunks_done") or None, timeout=300)
    time.sleep(3)
    proc.send_signal(signal.SIGINT)
    time.sleep(1.0)
    t0 = time.time()
    proc.send_signal(signal.SIGINT)
    try:
        code = proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        print("FAILED: the second SIGINT did not end the process")
        return 1
    elapsed = time.time() - t0
    lines2, valid2, torn2 = read_registry(impatient)
    print(f"two SIGINTs   -> exit={code} after {elapsed:.1f}s, {valid2} cues kept, {torn2} torn")
    if code != 2:
        failures.append(f"exit after two SIGINTs is {code}, expected 2")
    if elapsed > 10:
        failures.append(f"the second SIGINT took {elapsed:.1f}s to be honoured")
    if valid2 < 1 or torn2:
        failures.append("the second SIGINT lost or damaged what was already recorded")

    # --- the same command again: nothing recorded is redone, and it ends clean ------
    before = hashlib.sha256("\n".join(lines).encode()).hexdigest()
    proc = spawn(stopped)
    try:
        code = proc.wait(timeout=1800)
    except subprocess.TimeoutExpired:
        proc.kill()
        print("FAILED: the resumed run did not finish in 30 minutes")
        return 1
    lines3, valid3, torn3 = read_registry(stopped)
    after = hashlib.sha256("\n".join(lines3[: len(lines)]).encode()).hexdigest()
    record = read_record(stopped)
    print(
        f"resumed       -> exit={code} state={record.get('state')!r} "
        f"blocks={record.get('chunks_done')}/{record.get('chunks_total')} "
        f"cues={valid3} prefix={before[:16]}->{after[:16]}"
    )
    if before != after:
        failures.append("the resumed run rewrote the lines that were already recorded")
    if code != 0 or record.get("state") != "finished":
        failures.append(f"the resumed run ended with exit {code} and state {record.get('state')!r}")
    if record.get("chunks_done") != record.get("chunks_total"):
        failures.append("the resumed run did not reach the last block")
    if torn3:
        failures.append(f"the final registry has {torn3} unparsable line(s)")

    if failures:
        print()
        for failure in failures:
            print(f"FAILED: {failure}")
        return 1

    print("\nOK — Ctrl-C stops between blocks, asks for a look, and the next run resumes for free.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
