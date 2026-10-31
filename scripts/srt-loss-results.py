#!/usr/bin/env python3
"""Retain and qualify every planned SRT loss trial, using only local receipts."""

import argparse
import csv
import json
from pathlib import Path
import shutil
import sys


METRICS = [
    "video", "audio", "decode_msgs", "data_orig", "data_retx", "naks",
    "handshake_flags", "reordered", "orig_distinct", "in_time",
]
EXITS = ["encoder_exit", "sender_exit", "video_probe_exit", "audio_probe_exit",
         "decoder_exit", "receiver_exit", "proxy_exit"]
STOPS = ["sender_stop", "receiver_stop", "proxy_stop", "encoder_timeout"]
DIAGNOSTICS = ["rcv.log", "snd.log", "proxy.log", "proxy.txt", "ff.log", "pipeline.status",
               "outcomes.txt", "video.packets", "audio.packets", "video-probe.log",
               "audio-probe.log", "decode.log"]
HEADER = ["receiver", "loss", "latency_ms", "seed", *METRICS[:7], "status",
          *METRICS[7:], "encryption", "trial_index", "attempted", "failure",
          "trial_exit", *EXITS, *STOPS]


def number(value):
    text = str(value)
    return int(text) if text.isascii() and text.isdecimal() else None


def key_values(path):
    if not path.is_file():
        return {}
    return dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)


def packet_count(path):
    if not path.is_file():
        return None
    lines = path.read_text().splitlines()
    return number(lines[0]) if lines else None


def atomic_json(path, data):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def collect(work):
    outcomes = key_values(work / "outcomes.txt")
    outcomes.update(key_values(work / "pipeline.status"))
    proxy = key_values(work / "proxy.txt")
    metrics = {
        "video": packet_count(work / "video.packets"),
        "audio": packet_count(work / "audio.packets"),
        "decode_msgs": len((work / "decode.log").read_text().splitlines())
            if (work / "decode.log").is_file() else None,
        "handshake_flags": proxy.get("hs_flags", "NA"),
    }
    for field, source in [("data_orig", "data_orig"), ("data_retx", "data_retx"),
                          ("naks", "nak_packets"), ("reordered", "reordered_orig"),
                          ("orig_distinct", "orig_distinct"), ("in_time", "in_time")]:
        metrics[field] = number(proxy.get(source))
    return {"metrics": metrics, "outcomes": {
        field: number(outcomes.get(field)) for field in [*EXITS, *STOPS]
    }}


def qualify(raw, trial_exit, loss=None):
    """A completed source and decoded A/V are required for this A/V fixture.

    Decoder error messages fail a loss-free trial (or one whose loss is not
    known), where they mean the clip or harness is broken. Under loss they are
    what the receiver delivered: libsrt writes damaged bytes through, and that
    is the comparison, so they are recorded rather than disqualifying."""
    failures = []
    if trial_exit != 0:
        failures.append(f"trial exit {trial_exit if trial_exit is not None else 'missing'}")
    metrics = raw.get("metrics", {})
    outcomes = raw.get("outcomes", {})
    for field in EXITS:
        value = outcomes.get(field)
        if type(value) is not int or not 0 <= value <= 255:
            failures.append(f"{field} missing or invalid")
    for field in STOPS:
        if type(outcomes.get(field)) is not int or outcomes[field] not in (0, 1):
            failures.append(f"{field} missing or invalid")
    for field in ["encoder_exit", "video_probe_exit", "audio_probe_exit", "decoder_exit"]:
        if outcomes.get(field) != 0:
            failures.append(f"{field}={outcomes.get(field, 'missing')}")
    for child in ["sender", "receiver", "proxy"]:
        code = outcomes.get(child + "_exit")
        stopped = outcomes.get(child + "_stop") == 1
        if code != 0 and not (stopped and code in (130, 143)):
            failures.append(f"{child}_exit={code}")
    if outcomes.get("encoder_timeout") != 0:
        failures.append("encoder did not complete within the trial deadline")
    for field in ["video", "audio", "data_orig", "orig_distinct"]:
        value = metrics.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            failures.append(f"{field} must be positive")
    decode = metrics.get("decode_msgs")
    if type(decode) is not int or decode < 0 or (decode != 0 and loss in (None, "none")):
        failures.append(f"decode_msgs={decode}")
    for field in ["data_retx", "naks", "reordered", "in_time"]:
        value = metrics.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            failures.append(f"{field} missing or invalid")
    if isinstance(metrics.get("orig_distinct"), int) and isinstance(metrics.get("in_time"), int):
        if metrics["in_time"] > metrics["orig_distinct"]:
            failures.append("in_time exceeds orig_distinct")
    return failures


def collate(output, work=None):
    with (output / "plan.tsv").open(newline="") as stream:
        plan = list(csv.DictReader(stream, delimiter="\t"))
    indices = [row["trial_index"] for row in plan]
    if not plan or len(set(indices)) != len(indices) or any(number(i) is None for i in indices):
        raise ValueError("trial plan is empty or has invalid/duplicate indices")
    rows = []
    for planned in plan:
        index = planned["trial_index"]
        prefix = output / "trials" / index
        attempted = prefix.with_suffix(".started").is_file()
        raw = {}
        failures = []
        if attempted:
            try:
                raw = json.loads(prefix.with_suffix(".json").read_text())
                if not isinstance(raw, dict) or not isinstance(raw.get("metrics"), dict) or not isinstance(raw.get("outcomes"), dict):
                    raise ValueError("invalid receipt shape")
            except (OSError, ValueError) as error:
                # Preserve any partial tool outcomes from a killed shell, but
                # absence of its completed receipt still makes the row fail.
                raw = collect(work / f"t{index}") if work is not None else {}
                failures.append(f"missing or invalid trial receipt ({type(error).__name__})")
        exit_path = prefix.with_suffix(".exit")
        trial_exit = number(exit_path.read_text().strip()) if exit_path.is_file() else None
        if attempted:
            failures.extend(qualify(raw, trial_exit, planned["loss"]))
        else:
            failures.append("planned trial was not attempted")
        row = {field: planned[field] for field in ["receiver", "loss", "latency_ms", "seed"]}
        row.update(raw.get("metrics", {}))
        row.update(raw.get("outcomes", {}))
        row.update(trial_index=index, encryption=planned["encryption"] or "none",
                   attempted=int(attempted), trial_exit=trial_exit,
                   status=("FAIL" if attempted else "not_attempted") if failures else "ok",
                   failure="; ".join(failures))
        if attempted and failures and work is not None:
            diagnostics = output / "logs" / f"{index}-{planned['receiver']}"
            for name in DIAGNOSTICS:
                source = work / f"t{index}" / name
                if source.is_file():
                    diagnostics.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, diagnostics / name)
        rows.append(row)
    with (output / "results.tsv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=HEADER, delimiter="\t", lineterminator="\n", extrasaction="ignore")
        writer.writeheader()
        writer.writerows({field: "NA" if row.get(field) is None else row[field] for field in HEADER} for row in rows)
    result = {"schema": 1, "status": "passed" if all(row["status"] == "ok" for row in rows) else "failed",
              "planned": len(rows), "attempted": sum(row["attempted"] for row in rows),
              "passed": sum(row["status"] == "ok" for row in rows),
              "failed": sum(row["status"] == "FAIL" for row in rows),
              "not_attempted": sum(row["status"] == "not_attempted" for row in rows),
              "trials": rows}
    run_exit = output / "run-body.exit"
    result["runner_body_exit"] = number(run_exit.read_text().strip()) if run_exit.is_file() else None
    if result["runner_body_exit"] not in (None, 0):
        result["status"] = "failed"
    atomic_json(output / "qualification.json", result)
    return result["status"] == "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["record", "collate"])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--index", type=int)
    parser.add_argument("--work", type=Path)
    parser.add_argument("--loss")
    args = parser.parse_args()
    if args.command == "record":
        if args.index is None or args.work is None:
            parser.error("record needs --index and --work")
        raw = collect(args.work)
        atomic_json(args.output / "trials" / f"{args.index}.json", raw)
        return 0 if not qualify(raw, 0, args.loss) else 1
    return 0 if collate(args.output, args.work) else 1


if __name__ == "__main__":
    sys.exit(main())
