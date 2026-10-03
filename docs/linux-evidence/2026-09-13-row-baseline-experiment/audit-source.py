from pathlib import Path
import ast, gzip, hashlib, io, json, re, statistics, struct, subprocess, sys, tarfile

BASE=Path('/tmp/kokuban-perf-20260913/root-evidence/run-34748686411')
A=BASE/sys.argv[1]; ARCH='aarch64' if A.name.endswith('-arm') else 'x86_64'; HARNESS='62c99adc6fc1dbf12d89ee717f5823644c162d47'; D=A/'measurements'; REPO='/Users/eguimacs/kokuban.rs'
REFS={'before':'ddccc9bc4146629c34c089486d56897b0b3e3515','after':'6a49dfbafab15b27b3b554672fbc5fa9eb436d37'}
sha=lambda b:hashlib.sha256(b).hexdigest()
def archive_bytes(path):
 return path.read_bytes() if path.exists() else gzip.decompress(path.with_suffix(path.suffix+'.gz').read_bytes())
blob=lambda ref,path:subprocess.check_output(['git','show',ref+':'+path],cwd=REPO)
r=json.loads((D/'report.json').read_text()); injection=json.loads((A/'injection/injection.json').read_text())
assert r['status']=='completed' and r['pixel_equivalence_verified'] is True
assert r['comparison']==injection['comparison']=='revisions' and injection['common_source'] is None
assert r['machine']==ARCH and (A/'comparison.txt').read_text().strip()=='revisions'
assert r['settings']=={'pairs':6,'steps':300,'warmup':30,'samples_per_process':1}
assert r['cpu_affinity']==[0] and (A/'benchmark-cpu.txt').read_text().strip()=='0'
assert 'rustc 1.94.1' in (A/'rustc.txt').read_text()
assert (A/'harness-revision.txt').read_text().strip()==HARNESS
for local,remote in [('compare-frame-revisions.py','scripts/compare-frame-revisions.py'),('test_compare_frame_revisions.py','scripts/test_compare_frame_revisions.py'),('linux-render-performance.yml','.github/workflows/linux-render-performance.yml')]:
 assert (A/local).read_bytes()==blob(HARNESS,remote),local
assert sha((A/'compare-frame-revisions.py').read_bytes())==r['runner_sha256']
assert sha((A/'injection/injection.json').read_bytes())==r['injection_sha256']
benchmark=(A/'injection/benchmark.rs.txt').read_bytes()
assert sha(benchmark)==r['benchmark_sha256']==injection['benchmark_sha256']
assert sha(blob(HARNESS,'src/linux_window.rs'))==injection['harness_source_sha256']
source_audits={}; sources={}
for side,ref in REFS.items():
 assert (A/f'{side}-revision.txt').read_text().strip()==ref
 archive=A/f'{side}-original-source.tar'; digest=sha(archive_bytes(archive))
 assert digest==(A/f'{side}-source-sha256.txt').read_text().split()[0]
 tree={}
 for row in subprocess.check_output(['git','ls-tree','-r','-z',ref],cwd=REPO).split(b'\0'):
  if not row:continue
  metadata,path=row.split(b'\t',1); mode,kind,gitsha=metadata.decode().split()
  assert kind=='blob';tree[path.decode()]=(mode,gitsha)
 archived={}
 with tarfile.open(fileobj=io.BytesIO(archive_bytes(archive))) as tar:
  for m in tar:
   assert not m.name.startswith('/') and '..' not in Path(m.name).parts
   if m.isdir():continue
   assert m.name not in archived and m.name in tree,m.name
   assert m.isfile() or m.issym(),m.name
   data=tar.extractfile(m).read() if m.isfile() else m.linkname.encode()
   mode,gitsha=tree[m.name]
   assert (mode=='120000')==m.issym()
   assert hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()==gitsha,m.name
   archived[m.name]=data
 assert archived.keys()==tree.keys(),(side,set(tree)-set(archived))
 sources[side]=archived
 original_manifest=json.loads((A/f'injection/{side}/original-source-manifest.json').read_text())
 prepared_manifest=json.loads((A/f'injection/{side}/prepared-source-manifest.json').read_text())
 assert original_manifest=={name:sha(data) for name,data in archived.items()}
 prepared_tar=A/f'{side}-prepared-source.tar'
 assert sha(archive_bytes(prepared_tar))==(A/f'{side}-prepared-source-sha256.txt').read_text().split()[0]
 prepared={}
 with tarfile.open(fileobj=io.BytesIO(archive_bytes(prepared_tar))) as tar:
  for m in tar:
   if m.isdir():continue
   assert not m.name.startswith('/') and '..' not in Path(m.name).parts
   name=m.name.removeprefix('./')
   assert m.isfile() and name not in prepared
   prepared[name]=tar.extractfile(m).read()
 assert prepared==archived
 assert prepared_manifest==original_manifest==injection['sources'][side]['manifest']
 meta=injection['sources'][side];build=r['builds'][side]
 assert all(build[k]==v for k,v in meta.items())
 original=(A/f'injection/{side}/original-linux_window.rs').read_bytes()
 injected=(A/f'injection/{side}/injected-linux_window.rs').read_bytes()
 assert original==archived['src/linux_window.rs']==injected
 assert sha(original)==meta['original_sha256'] and sha(injected)==meta['injected_sha256']
 assert injected.endswith(benchmark) and injected.count(benchmark)==1
 assert sha(injected[:-len(benchmark)])==meta['preserved_prefix_sha256']
 for name,digest in meta['cargo_sha256'].items():assert sha(archived[name])==digest
 messages_path=A/f'{side}-cargo-messages.jsonl'
 assert sha(messages_path.read_bytes())==build['cargo_messages_sha256']
 messages=[json.loads(line) for line in messages_path.read_text().splitlines()]
 assert [m for m in messages if m['reason']=='build-finished']==[{'reason':'build-finished','success':True}]
 apps=[m for m in messages if m['reason']=='compiler-artifact' and m['target']['name']=='kokuban']
 assert len(apps)==1;app=apps[0]
 assert app['fresh'] is False and app['profile']=={'opt_level':'3','debuginfo':0,'debug_assertions':False,'overflow_checks':False,'test':True}
 assert app['executable']==build['binary'] and app['manifest_path']==build['source']+'/Cargo.toml'
 assert build['target']+'/release/deps/' in app['executable']
 assert not re.search(r'error:|panicked|FAILED',(A/f'{side}-build.log').read_text())
 binary=(A/f'{side}-benchmark').read_bytes()
 assert sha(binary)==build['binary_sha256']==(A/f'{side}-binary-sha256.txt').read_text().strip()==r['execution_binaries'][side]['sha256']
 assert r['execution_binaries'][side]['path']==build['binary']
 assert binary[:6]==b'\x7fELF\x02\x01' and struct.unpack_from('<H',binary,18)[0]==(183 if ARCH=='aarch64' else 62)
 source_audits[side]={'revision':ref,'archive_sha256':sha(archive_bytes(archive)),'git_blobs_verified':len(archived),'fresh_optimized_build_record_verified':True,'recorded_binary_sha256':build['binary_sha256'],'binary_bytes_retained':True,'ELF_machine':183 if ARCH=='aarch64' else 62,'original_and_prepared_manifest_verified':True}
assert r['builds']['before']['target']!=r['builds']['after']['target']
assert r['builds']['before']['binary_sha256']!=r['builds']['after']['binary_sha256']
assert sources['before'].keys()==sources['after'].keys()
changed=[p for p in sources['before'] if sources['before'][p]!=sources['after'][p]]
assert changed==['src/linux_window.rs'],changed
previous_harness=blob('1f2736405c9205a0ac765f98b5977f5d9a8f5581','src/linux_window.rs')
assert previous_harness.endswith(benchmark)

expected_modes={(c,g,i) for c in ('ascii','ascii-after-emoji','unicode') for g in ('single-row','full-screen') for i in (False,True)}
expected_frames={f'{c}-{g}-{i}.xrgb8888le' for c,g,_ in expected_modes for i in (0,1)}
fixture_re=re.compile(r'^frame-repaint fixture content=(\S+) change=(\S+) cols=(\d+) rows=(\d+) width=(\d+) height=(\d+) cell_width=(\d+) cell_height=(\d+) warmup=(\d+) samples=(\d+) steps=(\d+) mono_cells=(\d+) color_cells=(\d+) bands=(\[.*\]) frame0=([0-9a-f]{16}) frame1=([0-9a-f]{16})$')
time_re=re.compile(r'^frame-repaint sample content=(\S+) change=(\S+) sample=(\d+) incremental=(true|false) frames=(\d+) elapsed_ns=(\d+) ns_per_frame=([0-9.]+)$')
order=[(p,s) for p in range(1,7) for s in (('before','after') if p%2 else ('after','before'))]
assert [(e['pair'],e['side']) for e in r['execution_order']]==order
for e in r['execution_order']:
 assert e['command']==[r['builds'][e['side']]['binary'],'--exact','linux_window::damage_tests::benchmark_frame_repaint','--ignored','--nocapture','--test-threads=1']
assert {p.name for p in D.glob('*.log')}=={f'{p:02d}-{s}.log' for p,s in order}
parsed=[]; reference_fixtures=None; frame_fnv={}
filtered_counts={s:int(re.search(r'0 measured; (\d+) filtered out;', (D/f'01-{s}.log').read_text()).group(1)) for s in ('before','after')}
assert filtered_counts['after']==filtered_counts['before']+1
for pair,side in order:
 log=(D/f'{pair:02d}-{side}.log').read_text();filtered=filtered_counts[side]
 assert len(re.findall(r'^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; '+str(filtered)+r' filtered out; finished in [0-9.]+s$',log,re.M))==1
 assert not re.search(r'panicked|test result: FAILED|error:',log)
 fixtures={};seen=set();current=[]
 for line in log.splitlines():
  if 'frame-repaint fixture' in line:
   line=line[line.index('frame-repaint fixture'):];m=fixture_re.fullmatch(line);assert m,line
   c,g,*v=m.groups();key=(c,g);assert key not in fixtures;fixtures[key]=line;current.append(line)
   cols,rows,w,h,cw,ch,warm,samples,steps,mono,color=map(int,v[:11]);bands=ast.literal_eval(v[11]);f0,f1=v[12:]
   assert [cols,rows,w,h,cw,ch,warm,samples,steps]==[120,40,1080,680,9,17,30,1,300]
   assert [mono,color]==([2800,800] if c=='unicode' else [4800,0])
   assert bands==([(340,357)]*2 if g=='single-row' else [(0,680)]*2)
   assert f0!=f1
   for i,value in enumerate([f0,f1]):
    name=f'{c}-{g}-{i}.xrgb8888le'
    assert name not in frame_fnv or frame_fnv[name]==value
    frame_fnv[name]=value
  if 'frame-repaint sample' in line:
   m=time_re.fullmatch(line);assert m,line
   c,g,sample,inc,n,elapsed,rounded=m.groups();key=(c,g,inc=='true')
   assert key in expected_modes and key not in seen;seen.add(key)
   assert sample=='0' and int(n)==300 and int(elapsed)>0
   value=int(elapsed)/int(n);assert abs(float(rounded)-value)<=.000501
   parsed.append({'content':c,'change':g,'incremental':inc=='true','frames':int(n),'elapsed_ns':int(elapsed),'ns_per_frame':value,'pair':pair,'side':side})
 assert seen==expected_modes and set(fixtures)=={(c,g) for c,g,i in expected_modes}
 if reference_fixtures is None:reference_fixtures=current
 assert current==reference_fixtures==[f['line'] for f in r['fixtures']]
assert all(f['width']==1080 and f['height']==680 for f in r['fixtures'])
assert parsed==r['records'] and len(parsed)==144
old=json.loads((BASE.parent/'run-34744867099/independent-full-frame-repaint-audit.json').read_text())
golden=old['architectures']['x86_64']['phases']['candidate']['image_sha256']
fnv_cache={};frame_hashes={};pixels_before={}
for side in ('before','after'):
 folder=D/f'{side}-frames';assert {p.name for p in folder.iterdir()}==expected_frames
 for p in sorted(folder.iterdir()):
  data=p.read_bytes();assert len(data)==1080*680*4,(p,len(data))
  digest=sha(data);assert r['frame_sha256'][side][p.name]==digest==golden[p.name]
  if digest not in fnv_cache:
   h=0xcbf29ce484222325
   for byte in data:h=((h^byte)*0x100000001b3)&0xffffffffffffffff
   fnv_cache[digest]=f'{h:016x}'
  assert fnv_cache[digest]==frame_fnv[p.name]
  if side=='before':pixels_before[p.name]=data;frame_hashes[p.name]=digest
  else:assert pixels_before[p.name]==data
for c,g,_ in expected_modes:
 assert pixels_before[f'{c}-{g}-0.xrgb8888le']!=pixels_before[f'{c}-{g}-1.xrgb8888le']

stats=[]
for row in r['summary']:
 key=(row['content'],row['change'],row['incremental'])
 assert key in expected_modes
 values={s:[p['ns_per_frame'] for p in parsed if p['side']==s and (p['content'],p['change'],p['incremental'])==key] for s in ('before','after')}
 assert values==row['samples_ns']
 med={s:statistics.median(v) for s,v in values.items()};ranges={s:[min(v),max(v)] for s,v in values.items()}
 changes=[100*(a/b-1) for b,a in zip(values['before'],values['after'])]
 deltas_us=[(a-b)/1000 for b,a in zip(values['before'],values['after'])]
 ratio=100*(med['after']/med['before']-1)
 assert row['median_ns']==med and row['range_ns']==ranges
 assert changes==row['paired_latency_change_percent'] and row['latency_change_percent']==ratio
 assert row['slower_pairs']==sum(c>0 for c in changes)
 assert row['median_paired_latency_change_percent']==statistics.median(changes)
 stats.append({'content':key[0],'change':key[1],'incremental':key[2],
  'median_paired_time_change_percent':statistics.median(changes),'paired_time_change_percent':changes,
  'paired_time_change_range_percent':[min(changes),max(changes)],
  'AB_median_time_change_percent':statistics.median(changes[::2]),'BA_median_time_change_percent':statistics.median(changes[1::2]),
  'median_paired_time_delta_us':statistics.median(deltas_us),'before_median_us':med['before']/1000,'after_median_us':med['after']/1000,
  'before_range_us':[x/1000 for x in ranges['before']],'after_range_us':[x/1000 for x in ranges['after']],
  'slower_pairs':row['slower_pairs'],'ratio_of_separate_medians_time_change_percent':ratio})
assert len(stats)==len({(s['content'],s['change'],s['incremental']) for s in stats})==12
output={'status':'PASS_WITH_SCOPE_LIMITS','run_id':34748686411,'harness':HARNESS,'architecture':ARCH,'filtered_test_counts':filtered_counts,'scope':'Prewarmed CPU software repaint/damage work; excludes PTY, layout/font shaping, presentation and physical display latency.',
 'hardware':next(line.split(':',1)[1].strip() for line in (A/'cpu.txt').read_text().splitlines() if line.startswith('Model name:')),
 'sources':source_audits,'changed_original_source_files':changed,'common_benchmark_sha256':sha(benchmark),'benchmark_identical_to_1f2736405c9205a0ac765f98b5977f5d9a8f5581':True,
 'complete_logs':12,'timing_records':144,'timed_paint_iterations':43200,'pairs':6,'order':['AB','BA','AB','BA','AB','BA'],'same_binary_control':False,
 'frame_dimensions':[1080,680],'cell_dimensions':[9,17],'grid_dimensions':[120,40],'reference_images_checked':24,'reference_image_bytes_each':2937600,
 'frame_sha256':frame_hashes,'frame_fnv64':frame_fnv,'all_before_after_reference_pixels_identical':True,'all_two_state_scenes_distinct':True,'all_images_match_previous_run_34744867099':True,
 'summary':stats,'font_file_sha256':json.loads((A/'font-files.json').read_text()),'packages':(A/'system-packages.txt').read_text().splitlines(),
 'limits':['This run directly compares ddccc9b to 6a49dfb on the same host: reuse of a rounded per-row glyph baseline. Both builds already include the damage-band filter and ASCII cache. Percentages from other runs must not be added.',
 'Both original and prepared source archives, full manifests, fresh Cargo build records, retained executable SHA256 and ELF architecture are independently verified.',
 'The fixtures assert full/partial equivalence for both transitions outside timing and check final timed state; the artifacts do not capture every intermediate timed frame.',
 'This hardware differs from earlier macro and four-terminal runs; absolute timing comparisons across hosts are not causal.',
 'Font versions, file hashes and match queries are retained but font bytes are not; all actual reference pixels nevertheless match the independently audited previous macro.'],
 'recommendation':'Read each of the six paired changes, both order medians and their ranges. This isolates per-row baseline reuse over ddccc9b; input processing and presentation remain outside scope.'}
path=A/'independent-row-baseline-frame-audit.json';path.write_text(json.dumps(output,indent=2)+'\n')
print(path)
for s in stats:print(s['content'],s['change'],s['incremental'], 'paired%',round(s['median_paired_time_change_percent'],4),'AB%',round(s['AB_median_time_change_percent'],4),'BA%',round(s['BA_median_time_change_percent'],4),'delta_us',round(s['median_paired_time_delta_us'],3),'range',*[round(x,3) for x in s['paired_time_change_range_percent']])
print('PASS',len(stats),'modes,24 images,144 records; sources',source_audits)
