'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
test('静的・ライブとも管理した macmon を優先し、使えなければ PATH に戻る', (t) => {
  const py = process.platform === 'win32' ? 'python' : 'python3';
  if (spawnSync(py, ['--version']).status !== 0) return t.skip('Python is unavailable');
  const script = `import ast,os,tempfile,types\nfrom pathlib import Path\nwith tempfile.TemporaryDirectory() as tmp:\n managed=Path(tmp)/'macmon'\n for source in ['probes/mac_probe.py','probes/live_mac.py']:\n  definitions=[x for x in ast.parse(Path(source).read_text(encoding='utf-8')).body if isinstance(x,ast.FunctionDef) and x.name=='macmon_path']\n  assert len(definitions)==1,source\n  ns={'os':types.SimpleNamespace(path=types.SimpleNamespace(expanduser=lambda _:str(managed),isfile=os.path.isfile),access=os.access,X_OK=os.X_OK),'shutil':types.SimpleNamespace(which=lambda _:'/vendor/macmon')}\n  exec(compile(ast.Module(body=definitions,type_ignores=[]),source,'exec'),ns)\n  assert ns['macmon_path']()=='/vendor/macmon'\n  managed.write_text('fixture');managed.chmod(0o755)\n  assert ns['macmon_path']()==str(managed)\n  if os.name!='nt':\n   managed.chmod(0o644)\n   assert ns['macmon_path']()=='/vendor/macmon'\n  managed.unlink()\n  ns['shutil'].which=lambda _:None\n  assert ns['macmon_path']()==str(managed)\nprint('PASS')\n`;
  const r = spawnSync(py, ['-c', script], { cwd: path.join(__dirname, '..'), encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /PASS/);
});
test('Mac hardware details work with the minimal SSH PATH', {skip:process.platform!=='darwin'}, () => {
  const script = `import ast,json,subprocess\nfrom pathlib import Path\ns=ast.parse(Path('probes/mac_probe.py').read_text(encoding='utf-8'))\nns={'subprocess':subprocess}\nn=[x for x in s.body if isinstance(x,ast.FunctionDef) and x.name in ['run','sysctl']]\nexec(compile(ast.Module(body=n,type_ignores=[]),'mac_probe.py','exec'),ns)\nf=ns['sysctl']\nassert f('machdep.cpu.brand_string')\nassert int(f('hw.ncpu'))>0\nassert int(f('hw.memsize'))>0\nprint('PASS')\n`;
  const r=spawnSync('/usr/bin/python3',['-c',script],{cwd:path.join(__dirname,'..'),env:{...process.env,PATH:'/usr/bin:/bin'},encoding:'utf8'});
  assert.equal(r.status,0,r.stderr);
  assert.match(r.stdout,/PASS/);
});
test('macmon metrics keep scope, zero, missing and finite clock values', (t) => {
  const py = process.platform === 'win32' ? 'python' : 'python3';
  if (spawnSync(py,['--version']).status !== 0) return t.skip('Python is unavailable');
  const script = `import ast,math,json\nfrom pathlib import Path\ns=ast.parse(Path('probes/live_mac.py').read_text(encoding='utf-8'))\nns={'math':math}\nn=[x for x in s.body if isinstance(x,ast.FunctionDef) and x.name in ['sensor_number','macmon_metrics']]\nexec(compile(ast.Module(body=n,type_ignores=[]),'live_mac.py','exec'),ns)\nf=ns['macmon_metrics']\nr=f({'cpu_power':0,'gpu_power':3,'ane_power':1,'all_power':4,'sys_power':0,'gpu_scaled_ratio':0.25,'gpu_freq_mhz':500,'temp':{'gpu_temp_avg':40}})\nassert r['power']['package_w']==0\nassert r['power']['soc_w']==4\nassert r['power']['platform_w'] is None\nassert r['gpu'][0]['util']==25\nassert r['gpu'][0]['power_w']==3\nassert r['gpu'][0]['clocks_graphics_mhz']==500\nassert 'wall_w' not in r['power']\nr=f({'cpu_power':float('inf'),'gpu_power':-1,'all_power':float('nan'),'gpu_freq_mhz':float('inf')})\nassert r['power']['package_w'] is None\nassert r['power']['soc_w'] is None\nassert r['gpu'][0]['power_w'] is None\nassert r['gpu'][0]['clocks_graphics_mhz'] is None\nassert f([])=={}\nprint('PASS')\n`;
  const result=spawnSync(py,['-c',script],{cwd:path.join(__dirname,'..'),encoding:'utf8'});
  assert.equal(result.status,0,result.stderr);
  assert.match(result.stdout,/PASS/);
});
