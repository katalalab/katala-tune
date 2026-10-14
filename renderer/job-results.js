'use strict';
const JobResults = (() => {
  const SCHED_OK = new Set([0, ...Array.from({ length: 16 }, (_, i) => 0x41300 + i)]);
  // vicius: NV_S_UP_TO_DATE=202。root の正規 HidHide updater だけに適用。
  // https://github.com/nefarius/vicius/blob/8624f8911997a42d5c788ba9c1e45ab31da15477/src/Common.h
  const updaterCurrent = (j) => j.kind === 'schtask' && j.id === '\\nefarius_HidHide_Updater' && j.name === 'nefarius_HidHide_Updater' && Number(j.last_result) === 202;
  const isFailing = (j) => !['disabled', 'unknown', 'running'].includes(j.state) && j.last_result != null
    && (j.kind === 'schtask' ? !SCHED_OK.has(Number(j.last_result)) && !updaterCurrent(j) : j.scope === 'user' && j.last_result !== 0);
  const label = (j) => updaterCurrent(j) ? '最新（202）' : isFailing(j) ? `失敗 ${j.last_result}` : '成功';
  return { isFailing, label, SCHED_OK };
})();
if (typeof module !== 'undefined' && module.exports) module.exports = JobResults;
