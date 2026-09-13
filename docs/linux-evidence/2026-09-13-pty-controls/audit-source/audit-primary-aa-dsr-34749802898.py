#!/usr/bin/env python3
from pathlib import Path
import hashlib,json,math,re,statistics as st,subprocess,sys,gzip,io,tarfile,struct
BASE=Path('/private/tmp/kokuban-perf-20260913/root-evidence/run-34749802898')
A=BASE/'linux-paired-revision-measurements'; REPO='/Users/eguimacs/kokuban.rs'
REFS={'before':'d54f801e9dec41a6eb4f159397e45939c367531d','after':'d54f801e9dec41a6eb4f159397e45939c367531d'}
HARNESS='3ed42aae591bb53eeb3b28fb0270387b0e0c5a3d'
sha=lambda x:hashlib.sha256(x).hexdigest()
blob=lambda ref,path:subprocess.check_output(['git','show',ref+':'+path],cwd=REPO)
read=lambda name:json.loads((A/name).read_text())
r=read('measurements/report.json');run=json.loads((BASE/'github-run.json').read_text());log=(BASE/'github-run.log').read_text()
assert run['headSha']==HARNESS and run['conclusion']=='success' and run['status']=='completed'
assert r['status']=='passed' and r['pairs_requested']==5 and r['in_progress'] is None
assert r['backend']=='wayland' and r['screen']=='primary' and r['cpu_affinity']==[0]
assert r['requested_geometry']==[24,80] and r['font_pixels']==14.0 and r['scrollback_lines']==10000
assert r['bytes_requested_per_workload']==33554432 and r['timeout_seconds']==120 and r['settle_seconds']==1
assert r['binaries_identical'] and r['allow_identical_binaries'] and r['comparison_mode']=='same-executable-variability'
assert (A/'comparison.txt').read_text().strip()=='same-binary' and not (A/'after-build').exists()
assert r['source_refs_verified_by_runner'] is False and r['rendering_equivalence_verified'] is False
assert r['comparability']['paired_inputs_validated'] and r['comparability']['reasons']==[]
assert r['comparability']['geometries_rows_cols_pixels']==[[24,80,720,408]]
assert r['environment']['DISPLAY'] is None and r['environment']['WAYLAND_DISPLAY']=='wayland-kokuban-paired'
assert (A/'harness-revision.txt').read_text().strip()==HARNESS
for field,name in [('script_sha256','compare-kokuban-revisions.py'),('comparator_sha256','compare-terminal-performance.py'),('pty_driver_helpers_sha256','linux-resource-smoke.py')]:
 data=blob(HARNESS,'scripts/'+name);assert data==(BASE/'source-reference'/name).read_bytes() and sha(data)==r[field]
workflow=blob(HARNESS,'.github/workflows/linux-paired-performance.yml').decode()
assert workflow==(BASE/'source-reference/linux-paired-performance.yml').read_text()
assert '--target-dir "$RUNNER_TEMP/kokuban-paired-build-$label"' in workflow
assert 'git archive --format=tar "$revision" | gzip -n' in workflow
assert 'cargo build --release --locked' in workflow and 'labels=(before)' in workflow
assert len(re.findall(r'Compiling kokuban v0\.2\.0 \(/home/runner/work/_temp/kokuban-paired-source-before\)',(A/'before-build/build.log').read_text()))==1
assert len(re.findall(r'Finished `release` profile \[optimized\]',(A/'before-build/build.log').read_text()))==1
assert re.search(r'Ran \d+ tests in [0-9.]+s',log)
assert 'BEFORE_REF: '+REFS['before'] in log and 'AFTER_REF: '+REFS['after'] in log and 'SCREEN: primary' in log
assert 'rustc 1.94.1' in (A/'rustc.txt').read_text()
changed=subprocess.check_output(['git','diff','--name-only',REFS['before'],REFS['after'],'--','src','Cargo.toml','Cargo.lock'],cwd=REPO).decode().splitlines()
assert changed==[]
weston=(A/'weston.log').read_text();assert 'weston 13.0.0' in weston and 'Using Pixman renderer' in weston and '--backend=headless-backend.so' in weston
teardown_note='BUG: finalizing a layer with views still on it.'
assert weston.index('caught signal 15')<weston.index(teardown_note)
order=[(p,s) for p in range(1,6) for s in (('before','after') if p%2 else ('after','before'))]
assert [(v['pair'],v['side']) for v in r['execution_order']]==order
logged_order=[(int(p),s,ref) for p,s,ref in re.findall(r'pair (\d)/5: (before|after) \(([0-9a-f]{40})\)',log)]
assert [(p,s) for p,s,ref in logged_order]==order and all(ref==REFS[s] for p,s,ref in logged_order)
lines={'ascii':b'abcdefghijklmnopqrstuvwxyz0123456789 '*2+b'\r\n','ansi':b'\x1b[31mred\x1b[0m \x1b[1;34mblue\x1b[0m \x1b[38;2;80;200;90mtruecolor\x1b[0m\r\n','unicode':('café 日本 λ e\u0301 '*3+'\r\n').encode(),'short_lines':b'x\r\n'}
payloads={}
for name,line in lines.items():
 count=33554432//len(line);chunks,left=divmod(count,4096);h=hashlib.sha256();block=line*4096
 for _ in range(chunks):h.update(block)
 h.update(line*left)
 expected={'bytes':count*len(line),'sha256':h.hexdigest(),'line_bytes':len(line),'line_repetitions':count}
 assert all(r['payloads'][name][k]==expected[k] for k in ['bytes','sha256']);payloads[name]=expected
assert set(r['payloads'])==set(lines)
binary=(A/'before-build/kokuban').read_bytes(); binary_sha=sha(binary)
assert binary[:6]==b'\x7fELF\x02\x01' and struct.unpack_from('<H',binary,18)[0]==62
assert binary_sha==r['terminals']['before']['sha256']==r['terminals']['after']['sha256']
messages=[json.loads(line) for line in (A/'before-build/cargo-messages.jsonl').read_text().splitlines()]
assert [m for m in messages if m['reason']=='build-finished']==[{'reason':'build-finished','success':True}]
apps=[m for m in messages if m['reason']=='compiler-artifact' and m['target']['name']=='kokuban']
assert len(apps)==1; app=apps[0]
assert app['fresh'] is False and app['profile']=={'opt_level':'3','debuginfo':0,'debug_assertions':False,'overflow_checks':False,'test':False}
assert app['executable']=='/home/runner/work/_temp/kokuban-paired-build-before/release/kokuban'
assert app['manifest_path']=='/home/runner/work/_temp/kokuban-paired-source-before/Cargo.toml'
packed=(A/'before-source.tar.gz').read_bytes()
assert sha(packed)==(A/'before-source-sha256.txt').read_text().split()[0]
tarbytes=gzip.decompress(packed); tree={}
for row in subprocess.check_output(['git','ls-tree','-r','-z',REFS['before']],cwd=REPO).split(b'\0'):
 if not row:continue
 metadata,path=row.split(b'\t',1);mode,kind,digest=metadata.decode().split();assert kind=='blob';tree[path.decode()]=(mode,digest)
archived={}
with tarfile.open(fileobj=io.BytesIO(tarbytes)) as tar:
 for m in tar:
  assert not m.name.startswith('/') and '..' not in Path(m.name).parts
  if m.isdir():continue
  assert m.name in tree and m.name not in archived and (m.isfile() or m.issym())
  data=tar.extractfile(m).read() if m.isfile() else m.linkname.encode();mode,digest=tree[m.name]
  assert (mode=='120000')==m.issym()
  assert hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()==digest
  archived[m.name]=sha(data)
assert archived.keys()==tree.keys()
(A/'source-git-manifest.json').write_text(json.dumps(archived,indent=2)+'\n')
source_proof={'source_revision':REFS['before'],'compressed_archive_sha256':sha(packed),'compressed_archive_bytes':len(packed),'tar_sha256':sha(tarbytes),'tar_bytes':len(tarbytes),'git_blobs_verified':len(tree),'ELF_sha256':binary_sha,'ELF_bytes':len(binary),'ELF_machine':62,'fresh_cargo_artifact_json_verified':True,'Cargo_messages_sha256':sha((A/'before-build/cargo-messages.jsonl').read_bytes()),'one_build_only':True,'same_binary_path_both_labels':True}

configs=set();child_ids=set();terminal_ids=set();portal_logs=0;builds={}
for side,ref in REFS.items():
 terminal=r['terminals'][side]
 assert (A/f'{side}-revision.txt').read_text().strip()==ref==terminal['source_ref']
 assert terminal['path']=='/home/runner/work/_temp/kokuban-paired-binaries/before'
 assert terminal['version']=='kokuban 0.2.0' and terminal['version_observation']['status']==0
 reported=(A/'before-build/binary-sha256.txt').read_text().split()
 assert reported==[terminal['sha256'],terminal['path']]
 hashes={}
 for name in ['Cargo.toml','Cargo.lock']:
  data=(A/f'before-build/{name}').read_bytes();assert data==blob(ref,name);hashes[name]=sha(data)
 builds[side]={'source_revision':ref,'reported_binary_sha256':terminal['sha256'],'binary_path':terminal['path'],'binary_bytes_retained':True,'independent_ELF_rehash_possible':True,'cargo_inputs_sha256':hashes,'single_fresh_build_and_identical_execution_path_verified':True,'cargo_compiler_artifact_json_retained':True}
 assert len(terminal['samples'])==5
 for pair,sample in enumerate(terminal['samples'],1):
  assert sample['pair']==pair and sample['status']=='passed'
  folder=A/f'measurements/{pair:02d}-{side}'
  assert set(p.name for p in folder.iterdir())=={'case.json','child.json','finish','kokuban.toml','result.json','sample.json','terminal.log'}
  saved=json.loads((folder/'sample.json').read_text());assert saved=={k:v for k,v in sample.items() if k!='pair'}
  assert sample['command'][0]==terminal['path'] and sample['command'][1]=='-e'
  config=(folder/'kokuban.toml').read_bytes();assert sample['config']['text']==config.decode() and sample['config']['sha256']==sha(config)
  assert sample['config']['history_limit']==10000 and sample['config']['history_unit']=='lines'
  assert b'scrollback_lines = 10000' in config and b'enabled = false' in config;configs.add(sha(config))
  case=json.loads((folder/'case.json').read_text());assert case['screen']=='primary' and case['settle_seconds']==1.0 and case['payloads']=={n:r['payloads'][n]['path'] for n in lines}
  assert case['terminal_pid'] not in terminal_ids;terminal_ids.add(case['terminal_pid'])
  child=json.loads((folder/'child.json').read_text());identity=(child['pid'],child['start_ticks']);assert identity not in child_ids;child_ids.add(identity)
  text=(folder/'terminal.log').read_text();assert re.fullmatch(r'\[[^\n]+ ERROR sctk_adwaita::config\] XDG Settings Portal did not return response in time: timeout: 100ms, key: color-scheme\n',text);portal_logs+=1
  m=sample['measurements'];assert m==json.loads((folder/'result.json').read_text())
  assert m['initial_geometry']==[24,80,720,408] and m['term']=='xterm-256color'
  assert len(m['protocol_rtt_seconds'])==30 and all(math.isfinite(v) and v>0 for v in m['protocol_rtt_seconds'])
  assert list(m['workloads'])==list(lines)
  for name,o in m['workloads'].items():
   assert o['bytes']==payloads[name]['bytes'] and o['sha256']==payloads[name]['sha256']
   assert o['geometry_before']==o['geometry_after']==[24,80,720,408]
   assert o['reply_hex']=='1b5b313b3552'
   assert o['write_seconds']>0 and o['drain_rtt_seconds']>0 and o['terminal_cpu_seconds']>=0
   assert math.isclose(o['write_seconds']+o['drain_rtt_seconds'],o['write_and_dsr_seconds'],abs_tol=1e-12)
   assert math.isclose(o['mib_per_second'],o['bytes']/1024**2/o['write_and_dsr_seconds'],rel_tol=1e-12)
assert len(configs)==1 and builds['before']['cargo_inputs_sha256']==builds['after']['cargo_inputs_sha256']
assert builds['before']['reported_binary_sha256']==builds['after']['reported_binary_sha256']==binary_sha

def distribution(values):
 median=st.median(values)
 return {'count':len(values),'median':median,'min':min(values),'max':max(values),'median_absolute_deviation':st.median(abs(v-median) for v in values),'samples':values}
def check_distribution(values,reported):
 computed=distribution(values)
 for k,v in computed.items():
  if k=='samples':assert len(v)==len(reported[k]) and all(math.isclose(a,b,rel_tol=1e-10,abs_tol=1e-10) for a,b in zip(v,reported[k]))
  else:assert math.isclose(v,reported[k],rel_tol=1e-10,abs_tol=1e-10),(k,v,reported[k])
summary={}
for name in lines:
 observations={side:[s['measurements']['workloads'][name] for s in r['terminals'][side]['samples']] for side in REFS}
 before,after=observations['before'],observations['after']
 ratios=[a['mib_per_second']/b['mib_per_second'] for b,a in zip(before,after)]
 elapsed_ratios=[a['write_and_dsr_seconds']/b['write_and_dsr_seconds'] for b,a in zip(before,after)]
 changes=[(x-1)*100 for x in elapsed_ratios];throughputs=[(x-1)*100 for x in ratios]
 for metric,values in [('after_over_before_throughput_ratio',ratios),('after_over_before_elapsed_ratio',elapsed_ratios),('elapsed_change_percent',changes)]:check_distribution(values,r['paired_summary'][name][metric])
 for side in REFS:
  for metric in ['mib_per_second','write_and_dsr_seconds','terminal_cpu_seconds']:check_distribution([x[metric] for x in observations[side]],r['terminals'][side]['summary'][name][metric])
 summary[name]={'paired_elapsed_change_percent':distribution(changes),'paired_throughput_change_percent':distribution(throughputs),'AB_median_elapsed_change_percent':st.median(changes[::2]),'BA_median_elapsed_change_percent':st.median(changes[1::2]),'AB_median_throughput_change_percent':st.median(throughputs[::2]),'BA_median_throughput_change_percent':st.median(throughputs[1::2]),'faster_pairs':sum(v<0 for v in changes),'slower_pairs':sum(v>0 for v in changes),'median_paired_elapsed_delta_ms':st.median((a['write_and_dsr_seconds']-b['write_and_dsr_seconds'])*1000 for b,a in zip(before,after)),
 'by_revision':{side:{metric:distribution([x[metric] for x in observations[side]]) for metric in ['mib_per_second','write_and_dsr_seconds','drain_rtt_seconds','terminal_cpu_seconds','terminal_rss_before_kib','terminal_rss_after_kib']} for side in REFS}}
protocol={}
for side in REFS:
 samples=r['terminals'][side]['samples'];values=[x for s in samples for x in s['measurements']['protocol_rtt_seconds']];check_distribution(values,r['terminals'][side]['protocol_rtt_seconds'])
 protocol[side]={'pooled_seconds':distribution(values),'per_process_median_seconds':[st.median(s['measurements']['protocol_rtt_seconds']) for s in samples]}
protocol['paired_process_median_change_percent']=distribution([(a/b-1)*100 for b,a in zip(protocol['before']['per_process_median_seconds'],protocol['after']['per_process_median_seconds'])])
hardware=next(line.split(':',1)[1].strip() for line in (A/'cpu.txt').read_text().splitlines() if line.startswith('Model name:'))
output={'status':'PASS_WITH_PROVENANCE_AND_SCOPE_LIMITS','run_id':34749802898,'url':run['url'],'comparison':{'before':REFS['before'],'after':REFS['after'],'harness':HARNESS,'changed_production_files':changed},'screen':'primary','configured_history_lines':10000,'hardware':hardware,'cpu_affinity':[0],'native_backend':'Weston 13.0.0 headless Pixman/Wayland; DISPLAY absent','geometry_rows_columns_width_height':[24,80,720,408],'cell_pixels':[9,17],'font_match':r['font_match'],'packages':(A/'system-packages.txt').read_text().splitlines(),'builds':builds,'source_proof':source_proof,'payloads':payloads,'processes':10,'workload_observations':40,'pairs':5,'order':['AB','BA','AB','BA','AB'],'payloads_regenerated_by_hash_only':True,'DSR_reply_hex':'1b5b313b3552','all_config_sample_result_files_match':True,'all_report_distributions_independently_recomputed':True,'workloads':summary,'protocol_rtt':protocol,'nonfatal_logs':{'portal_timeouts_during_startup':portal_logs,'weston_teardown_message':teardown_note,'context':'Portal timeout is the sole message in every terminal log, before timed warmups; sample completion succeeded. Weston teardown warning followed SIGTERM after measurement completion.'},'recommendation':'Same-binary primary control: descriptive variability only. Compare with the separate A/B as context, not by subtracting medians or transferring a causal noise bound between hosts.','limits':[
 'The control built one d54f801 executable. Retained ELF SHA/architecture, compressed and raw source-tar SHA, every Git blob and a fresh optimized non-test Cargo compiler-artifact record were independently verified. Both labels use the same path and SHA; there is no code delta in this A/A run.',
 'Payload .bin files were excluded by upload. Exact deterministic payload bytes and SHA256 were regenerated without running the terminal; every report observation agrees.',
 'Each workload sends one full warmup payload and clears history/screen before timing. The primary history limit is configured to 10000 lines; actual final text/history contents and rendered pixels are not independently inspected by this DSR workflow.',
 'Write plus final DSR measures PTY/terminal processing and scheduling, not full-frame CPU repaint, GPU, keyboard-to-screen or physical presentation. The terminal may render concurrently, but this does not isolate that cost or prove presentation of every line.',
 'Five paired processes share one runner/CPU. AB appears three times and BA twice; both order medians and all outliers are retained. This run itself is a same-binary control; absolute numbers from other runners are not causal comparisons.',
 'Terminal CPU/RSS are reported process observations; raw terminal tick endpoints are absent. CPU tick resolution is 10 ms and RSS is sampled after warmup/history operations, not a measured peak or net-memory attribution.',
 'RTT pooling contains 150 observations per side but only five process samples. The five initial RTT warmups per process are excluded; protocol RTT is not presentation latency.',
 'All original ZIP bytes and local file hashes are retained; ZIP SHA256 matched GitHub API metadata. Artifacts normally expire after seven days.'
 ]}
(BASE/'independent-primary-aa-dsr-audit.json').write_text(json.dumps(output,indent=2)+'\n')
for name,s in summary.items():print(name,'elapsed%',round(s['paired_elapsed_change_percent']['median'],6),'throughput%',round(s['paired_throughput_change_percent']['median'],6),'AB%',round(s['AB_median_elapsed_change_percent'],6),'BA%',round(s['BA_median_elapsed_change_percent'],6),'ms',round(s['median_paired_elapsed_delta_ms'],6),'slower',s['slower_pairs'])
print('protocol process median before/after us',[[round(x*1e6,3) for x in protocol[side]['per_process_median_seconds']] for side in REFS]);print('protocol paired median%',protocol['paired_process_median_change_percent']['median'])
print('PASS',BASE/'independent-primary-aa-dsr-audit.json')
