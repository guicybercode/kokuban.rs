import json
from pathlib import Path
import runpy
import sys
import time
import tty

helpers = runpy.run_path('/work-scripts/linux-resource-smoke.py')
tty.setraw(0)
helpers['barrier'](b'idle')
start = time.time()
time.sleep(3)
end = time.time()
Path(sys.argv[1]).write_text(json.dumps({'idle_start_epoch': start, 'idle_end_epoch': end})+'\n')
