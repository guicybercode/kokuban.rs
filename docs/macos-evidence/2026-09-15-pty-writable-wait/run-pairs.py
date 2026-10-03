import json, os, statistics, subprocess, sys
before, after, pairs, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
env = dict(os.environ, KOKUBAN_PTY_WRITE_MIB="8", KOKUBAN_PTY_WRITE_SAMPLES="3")
def run(binary):
    text = subprocess.run([binary, "--ignored", "--nocapture", "--exact", "pty::unix::tests::benchmark_large_pty_write"],
                          capture_output=True, text=True, check=True, env=env).stdout
    rates = [float(line.split(",")[2]) for line in text.splitlines() if line[:1].isdigit() and line.count(",") == 2]
    assert len(rates) == 3, text
    return {"median_mib_s": statistics.median(rates), "samples_mib_s": rates}
results = []
for pair in range(1, pairs + 1):
    order = ("before", "after") if pair % 2 else ("after", "before")
    row = {"pair": pair, "order": order}
    for side in order:
        row[side] = run(before if side == "before" else after)
        print(pair, side, row[side]["median_mib_s"], flush=True)
    results.append(row)
changes = [(r["after"]["median_mib_s"] / r["before"]["median_mib_s"] - 1) * 100 for r in results]
summary = {"before_median_mib_s": statistics.median(r["before"]["median_mib_s"] for r in results),
           "after_median_mib_s": statistics.median(r["after"]["median_mib_s"] for r in results),
           "throughput_change_percent": {"median": statistics.median(changes), "min": min(changes), "max": max(changes),
                                         "pairs_slower": sum(c < 0 for c in changes), "samples": changes}}
json.dump({"pairs": results, "summary": summary}, open(out, "w"), indent=2)
print(json.dumps(summary, indent=1))
