import subprocess, sys, statistics, json
a, b, pairs, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
def run(binary):
    text = subprocess.run([binary, "16", "5"], capture_output=True, text=True, check=True).stdout
    rows = [l.split(",") for l in text.splitlines()[2:] if "," in l]
    return {r[0]: float(r[1]) for r in rows}, text
results = []
for pair in range(1, pairs + 1):
    order = [("before", a), ("after", b)] if pair % 2 else [("after", b), ("before", a)]
    sample = {}
    for side, binary in order:
        sample[side], sample[side + "_raw"] = run(binary)
    results.append(sample)
summary = {}
for w in results[0]["before"]:
    changes = [(r["after"][w] / r["before"][w] - 1) * 100 for r in results]
    summary[w] = {"throughput_change_percent_median": round(statistics.median(changes), 2),
                  "min": round(min(changes), 2), "max": round(max(changes), 2),
                  "pairs_slower": sum(c < 0 for c in changes),
                  "before_median_MiB_s": statistics.median(r["before"][w] for r in results),
                  "after_median_MiB_s": statistics.median(r["after"][w] for r in results)}
json.dump({"pairs": results, "summary": summary}, open(out, "w"), indent=2)
print(json.dumps(summary, indent=1))
