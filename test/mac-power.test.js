'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
test('macmon metrics keep scope, zero, missing and finite clock values', (t) => {
  const py = process.platform === 'win32' ? 'python' : 'python3';
  if (spawnSync(py,['--version']).status !== 0) return t.skip('Python is unavailable');
  const script = `import ast,math,json\nfrom pathlib import Path\ns=ast.parse(Path('probes/live_mac.py').read_text(encoding='utf-8'))\nns={'math':math}\nn=[x for x in s.body if isinstance(x,ast.FunctionDef) and x.name in ['sensor_number','macmon_metrics']]\nexec(compile(ast.Module(body=n,type_ignores=[]),'live_mac.py','exec'),ns)\nf=ns['macmon_metrics']\nr=f({'cpu_power':0,'gpu_power':3,'ane_power':1,'all_power':4,'sys_power':0,'gpu_scaled_ratio':0.25,'gpu_freq_mhz':500,'temp':{'gpu_temp_avg':40}})\nassert r['power']['package_w']==0\nassert r['power']['soc_w']==4\nassert r['power']['platform_w'] is None\nassert r['gpu'][0]['util']==25\nassert r['gpu'][0]['power_w']==3\nassert r['gpu'][0]['clocks_graphics_mhz']==500\nassert 'wall_w' not in r['power']\nr=f({'cpu_power':float('inf'),'gpu_power':-1,'all_power':float('nan'),'gpu_freq_mhz':float('inf')})\nassert r['power']['package_w'] is None\nassert r['power']['soc_w'] is None\nassert r['gpu'][0]['power_w'] is None\nassert r['gpu'][0]['clocks_graphics_mhz'] is None\nassert f([])=={}\nprint('PASS')\n`;
  const result=spawnSync(py,['-c',script],{cwd:path.join(__dirname,'..'),encoding:'utf8'});
  assert.equal(result.status,0,result.stderr);
  assert.match(result.stdout,/PASS/);
});
