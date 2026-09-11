import hashlib
import json
from pathlib import Path
import platform
import runpy
import statistics
import sys
from types import SimpleNamespace

script, before, after, destination = map(Path, sys.argv[1:])
helpers = runpy.run_path(str(script))
output = destination.resolve()
output.mkdir(parents=True, exist_ok=False)
args = SimpleNamespace(samples=5, screen='alternate', scrollback_lines=10000,
    ghostty_scrollback_bytes=67108864, font_pixels=14.0, columns=80, rows=24,
    backend='wayland', settle_seconds=1.0, timeout=30.0)
data = helpers['payloads'](4 * 1024 * 1024)
paths = {}
for name, payload in data.items():
    path = output / (name + '.bin')
    path.write_bytes(payload)
    paths[name] = str(path)
report = {'schema':1, 'scope':'Paired Kokuban processing throughput/DSR, not frame presentation',
    'platform':platform.platform(), 'samples_per_binary':args.samples,
    'source_revisions':{'before':'b58e499','after':'c51c006'},
    'screen':args.screen, 'requested_geometry':[args.rows,args.columns],
    'environment_note':'Ubuntu 26.04 arm64 Docker/Colima on Apple M4 macOS; Weston14 headless pixman/llvmpipe; no task builds or other task benchmarks during sampling',
    'runner_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    'comparison_script_sha256':hashlib.sha256(script.read_bytes()).hexdigest(),
    'font_match':helpers['command_observation'](['fc-match','-f','%{family}\n%{file}\n','DejaVu Sans Mono']),
    'payloads':{name:{'bytes':len(payload),'sha256':hashlib.sha256(payload).hexdigest()} for name,payload in data.items()},
    'execution_order':[], 'terminals':{}}
for name,path in [('before',before),('after',after)]:
    report['terminals'][name]={'path':str(path),'version':helpers['command_observation']([str(path),'--version'])['output'],
        'sha256':hashlib.sha256(path.read_bytes()).hexdigest(), 'samples':[]}
for index in range(args.samples):
    for name in (['before','after'] if index%2==0 else ['after','before']):
        terminal = report['terminals'][name]
        report['execution_order'].append([index+1,name])
        print(f'sample {index+1}/{args.samples}: {name}',flush=True)
        sample = helpers['execute_sample']('kokuban',Path(terminal['path']),terminal['version'],
            output/f'{index+1:02d}-{name}',paths,args)
        terminal['samples'].append(sample)
        helpers['record'](output,'report.json',report)
helpers['summarize'](report,args)
helpers['record'](output,'report.json',report)
print(json.dumps({name:{w:v['mib_per_second']['median'] for w,v in t['summary'].items()} for name,t in report['terminals'].items()},indent=2))
if any(s['status']!='passed' for t in report['terminals'].values() for s in t['samples']):
    raise SystemExit(1)
