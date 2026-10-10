'use strict';

function watts(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function finite(value) {
  return Number.isFinite(value) ? value : null;
}

function currency(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 8 ? value : null;
}

function summarize(snapshot = {}, settings = {}) {
  const power = snapshot?.power && typeof snapshot.power === 'object' ? snapshot.power : {};
  const gpuRows = Array.isArray(snapshot?.gpus) ? snapshot.gpus : null;
  const gpuValues = gpuRows?.map((gpu) => watts(gpu?.power_w));
  const gpuW = gpuValues && gpuValues.every((value) => value !== null)
    ? finite(gpuValues.reduce((total, value) => total + value, 0))
    : null;
  const socW = watts(power.soc_w);
  const cpuW = socW === null ? watts(power.package_w) : null;
  const wallW = watts(power.wall_w);
  const baseW = watts(settings?.base_w);
  const efficiency = settings?.psu_efficiency;
  const validEfficiency = typeof efficiency === 'number' && Number.isFinite(efficiency) && efficiency > 0 && efficiency <= 1;
  const missing = [];
  let totalW = null;
  let kind = 'missing';
  let source = null;

  if (wallW !== null) {
    totalW = wallW;
    kind = 'measured';
    source = 'wall';
  } else if (socW !== null && baseW !== null && validEfficiency) {
    totalW = finite((socW + baseW) / efficiency);
    if (totalW !== null) {
      kind = 'estimated';
      source = 'soc';
    } else missing.push('total_w');
  } else if (cpuW !== null && gpuW !== null && baseW !== null && validEfficiency) {
    totalW = finite((cpuW + gpuW + baseW) / efficiency);
    if (totalW !== null) {
      kind = 'estimated';
      source = 'components';
    } else missing.push('total_w');
  } else {
    if (socW === null && gpuW === null) missing.push('gpu_w');
    if (socW === null && cpuW === null) missing.push('cpu_w');
    if (baseW === null) missing.push('base_w');
    if (!validEfficiency) missing.push('psu_efficiency');
  }

  const hours = watts(settings?.hours);
  const rate = watts(settings?.rate_per_kwh);
  const projectedKwh = totalW !== null && hours !== null ? finite(totalW * hours / 1000) : null;
  const projectedCost = projectedKwh !== null && rate !== null ? finite(projectedKwh * rate) : null;
  return {
    gpu_w: socW === null ? gpuW : null,
    cpu_w: cpuW,
    soc_w: socW,
    wall_w: wallW,
    total_w: totalW,
    kind,
    source,
    missing,
    projected_kwh: projectedKwh,
    projected_cost: projectedCost,
    currency: currency(settings?.currency),
  };
}

function point(value) {
  if (!value || typeof value !== 'object'
    || typeof value.t !== 'number' || !Number.isFinite(value.t)
    || !Number.isInteger(value.seq)
    || typeof value.epoch !== 'string' || value.epoch.length === 0
    || typeof value.source !== 'string' || value.source.length === 0) return null;
  return { ...value, watts: watts(value.watts) };
}

function integrate(points, intervalSeconds = 1) {
  const interval = typeof intervalSeconds === 'number' && Number.isFinite(intervalSeconds) && intervalSeconds > 0
    ? intervalSeconds : 1;
  let kwh = 0;
  let coveredSeconds = 0;
  let durationSeconds = 0;
  let previous = null;
  let activeEpoch = null;
  const watermarks = new Map();
  const retiredEpochs = new Set();

  for (const raw of Array.isArray(points) ? points : []) {
    const current = point(raw);
    if (!current) {
      previous = null;
      continue;
    }
    if (retiredEpochs.has(current.epoch)) continue;
    const currentKey = JSON.stringify([current.epoch, current.source]);
    const watermark = watermarks.get(currentKey);
    if (watermark && (current.seq <= watermark.seq || current.t <= watermark.t)) continue;
    if (activeEpoch !== current.epoch) {
      if (activeEpoch !== null) retiredEpochs.add(activeEpoch);
      activeEpoch = current.epoch;
    }
    if (!previous) {
      previous = current;
      watermarks.set(currentKey, current);
      continue;
    }
    if (previous && (previous.epoch !== current.epoch || previous.source !== current.source)) {
      previous = current;
      watermarks.set(currentKey, current);
      continue;
    }
    if (previous && (current.seq <= previous.seq || current.t <= previous.t)) continue;
    if (previous) {
      const seconds = (current.t - previous.t) / 1000;
      durationSeconds += seconds;
      if (seconds <= interval * 3 && previous.watts !== null && current.watts !== null) {
        coveredSeconds += seconds;
        kwh += (previous.watts + current.watts) * seconds / 7200000;
      }
    }
    previous = current;
    watermarks.set(currentKey, current);
  }
  return {
    kwh: finite(kwh),
    covered_seconds: finite(coveredSeconds),
    duration_seconds: finite(durationSeconds),
    coverage: durationSeconds === 0 || !Number.isFinite(durationSeconds) || !Number.isFinite(coveredSeconds)
      ? (durationSeconds === 0 ? 0 : null)
      : finite(coveredSeconds / durationSeconds),
  };
}

module.exports = { summarize, integrate };
