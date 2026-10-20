'use strict';
// task-completion.v1 は申告を表示するだけで、ここでは完了・受入を判定しない。
(() => {
  const GATES = ['implementation', 'verification', 'integration', 'deployment', 'acceptance'];
  const DELIVERY = new Set(['local', 'repository', 'environment']);
  const STATUS = {
    pending: '未着手の申告', in_progress: '進行中の申告', blocked: '停止中の申告', unverified: '未照合の申告',
    pass: '成功の申告（未照合）', fail: '失敗の申告（未照合）',
  };
  const missing = null;
  const MAX_COVERAGE_ITEMS = 10000;

  function text(value, name, optional = false) {
    if (value === undefined) return optional ? missing : null;
    if (typeof value !== 'string') throw Error(`${name} は文字列で指定してください`);
    return value || missing;
  }

  function statusLabel(status) {
    if (status === undefined) return missing;
    if (typeof status !== 'string') throw Error('gate status は文字列で指定してください');
    return Object.hasOwn(STATUS, status) ? STATUS[status] : `未対応の申告値: ${status}`;
  }

  function evidence(value, task, now, counts, gateName) {
    if (value === undefined) return [];
    if (!Array.isArray(value)) throw Error('evidence は配列で指定してください');
    if (value.length > 100) throw Error('gate ごとの evidence は100件までです');
    counts.evidence += value.length;
    if (counts.evidence > 4000) throw Error('evidence は文書全体で4000件までです');
    return value.map((item) => {
      if (!item || typeof item !== 'object' || Array.isArray(item)) throw Error('evidence の項目は object で指定してください');
      const ref = text(item.ref, 'evidence ref', true);
      const result = text(item.result, 'evidence result', true);
      const observedAt = text(item.observed_at, 'evidence observed_at', true);
      const revision = text(item.revision, 'evidence revision', true);
      const environment = text(item.environment, 'evidence environment', true);
      const warnings = [];
      if (observedAt !== missing) {
        const when = isoTime(observedAt);
        if (when == null) warnings.push('日時が不正です');
        else if (when > now.getTime()) warnings.push('未来の日時です');
      }
      if (task.revision !== missing && revision !== missing && revision !== task.revision) warnings.push(`revision が対象と異なります: ${revision}`);
      if ((gateName === 'deployment' || gateName === 'acceptance') && task.target !== missing && environment !== missing && environment !== task.target) warnings.push(`対象環境が対象と異なります: ${environment}`);
      return { ref, result, observedAt, revision, environment, link: githubUrl(ref), warnings };
    });
  }

  function isoTime(value) {
    const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$/.exec(value);
    if (!match) return null;
    const [, year, month, day, hour, minute, second, zone] = match;
    const y = Number(year), m = Number(month), d = Number(day);
    if (m < 1 || m > 12 || d < 1 || d > new Date(Date.UTC(y, m, 0)).getUTCDate() || Number(hour) > 23 || Number(minute) > 59 || Number(second) > 59) return null;
    if (zone !== 'Z' && (Number(zone.slice(1, 3)) > 23 || Number(zone.slice(4, 6)) > 59)) return null;
    const time = Date.parse(value);
    return Number.isFinite(time) ? time : null;
  }

  function githubUrl(value) {
    if (typeof value !== 'string' || /[%\x00-\x1f\x7f?#]/.test(value)) return null;
    try {
      const url = new URL(value);
      if (url.href !== value || url.protocol !== 'https:' || url.hostname !== 'github.com' || url.username || url.password || url.port || url.search || url.hash) return null;
      return /^\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/(?:pull|issues)\/[1-9]\d*$/.test(url.pathname) ? url.href : null;
    } catch { return null; }
  }

  function stringsAndDepth(value, depth = 0, seen = new Set()) {
    if (typeof value === 'string') {
      if (value.length > 8192) throw Error('文字列は8192文字までです');
      return;
    }
    if (!value || typeof value !== 'object') return;
    if (depth > 16 || seen.has(value)) throw Error('ネストは16段までです');
    seen.add(value);
    for (const key of Object.keys(value)) {
      if (key.length > 8192) throw Error('文字列は8192文字までです');
      stringsAndDepth(value[key], depth + 1, seen);
    }
    seen.delete(value);
  }

  function stringList(value, name) {
    if (value === undefined) return [];
    if (!Array.isArray(value) || value.some((item) => typeof item !== 'string' || !item)) throw Error(`${name} は文字列の配列で指定してください`);
    return value;
  }

  function coverage(value) {
    if (value === undefined) return { declared: false, expectedDeclared: false, observedDeclared: false, unobservedDeclared: false, expected: [], observed: [], unobserved: [], missing: [], extra: [], overlap: [], unobservedOutsideExpected: [], duplicates: [], insufficient: [] };
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('coverage は object で指定してください');
    const expected = stringList(value.expected, 'coverage expected');
    const observed = stringList(value.observed, 'coverage observed');
    const unobserved = stringList(value.unobserved, 'coverage unobserved');
    if (expected.length + observed.length + unobserved.length > MAX_COVERAGE_ITEMS) throw Error('coverage は合計10000件までです');
    const duplicates = [];
    for (const [name, list] of [['expected', expected], ['observed', observed], ['unobserved', unobserved]]) {
      const seen = new Set();
      for (const item of list) { if (seen.has(item)) duplicates.push(`${name}: ${item}`); seen.add(item); }
    }
    const insufficient = [];
    if (!expected.length) insufficient.push('想定範囲が未記載です');
    if (!observed.length) insufficient.push('観測範囲が未記載です');
    const expectedSet = new Set(expected);
    const observedSet = new Set(observed);
    const unobservedSet = new Set(unobserved);
    const overlap = [...observedSet].filter((item) => unobservedSet.has(item));
    const unobservedOutsideExpected = [...unobservedSet].filter((item) => !expectedSet.has(item));
    return { declared: true, expectedDeclared: Object.hasOwn(value, 'expected'), observedDeclared: Object.hasOwn(value, 'observed'), unobservedDeclared: Object.hasOwn(value, 'unobserved'), expected, observed, unobserved, missing: [...expectedSet].filter((item) => !observedSet.has(item)), extra: [...observedSet].filter((item) => !expectedSet.has(item)), overlap, unobservedOutsideExpected, duplicates, insufficient };
  }

  function review(value, task, now) {
    if (value === undefined) return { status: undefined, acceptedLabel: missing, ref: missing, observedAt: missing, revision: missing, target: missing, link: null, warnings: [] };
    if (!value || typeof value !== 'object' || Array.isArray(value) || (value.status !== undefined && typeof value.status !== 'string')) throw Error('review status は文字列で指定してください');
    const status = value.status === undefined ? missing : value.status;
    const acceptedLabel = status === 'accepted' ? '受入の申告（未照合）' : status === 'pending' ? '受入待ちの申告' : status === missing ? missing : `未対応の申告値: ${status}`;
    const ref = text(value.ref, 'review ref', true);
    const observedAt = text(value.observed_at, 'review observed_at', true);
    const revision = text(value.revision, 'review revision', true);
    const target = text(value.target, 'review target', true);
    const warnings = [];
    if (observedAt !== missing) {
      const when = isoTime(observedAt);
      if (when == null) warnings.push('レビュー日時が不正です');
      else if (when > now.getTime()) warnings.push('レビュー日時が未来です');
    }
    if (task.revision !== missing && revision !== missing && revision !== task.revision) warnings.push(`review revision が対象と異なります: ${revision}`);
    if (task.target !== missing && target !== missing && target !== task.target) warnings.push(`review target が対象と異なります: ${target}`);
    if (status === 'accepted') {
      for (const [label, field] of [['review ref', ref], ['review observed_at', observedAt], ['review revision', revision], ['review target', target]]) {
        if (field === missing) warnings.push(`受入の申告に ${label} がありません`);
      }
    }
    return { status: value.status, acceptedLabel, ref, observedAt, revision, target, link: githubUrl(ref), warnings };
  }

  function parse(input, now = new Date()) {
    stringsAndDepth(input);
    if (!input || typeof input !== 'object' || Array.isArray(input) || input.schema !== 'task-completion.v1' || !Array.isArray(input.tasks)) throw Error('task-completion.v1 の tasks 配列が必要です');
    if (input.tasks.length < 1 || input.tasks.length > 1000) throw Error('tasks は1件から1000件までです');
    const ids = new Set();
    const counts = { evidence: 0 };
    const tasks = input.tasks.map((task) => {
      if (!task || typeof task !== 'object' || Array.isArray(task)) throw Error('task は object で指定してください');
      const id = typeof task.id === 'string' ? task.id.trim() : task.id;
      if (typeof id !== 'string' || !id || ids.has(id)) throw Error('task id は空でない一意の文字列で指定してください');
      ids.add(id);
      const taskView = { id, goal: text(task.goal, 'goal', true), target: text(task.target, 'target', true), revision: text(task.revision, 'revision', true), delivery: text(task.delivery, 'delivery', true) };
      if (task.gates !== undefined && (!task.gates || typeof task.gates !== 'object' || Array.isArray(task.gates))) throw Error('gates は object で指定してください');
      if (task.delivery !== undefined && (!DELIVERY.has(task.delivery))) throw Error('delivery は local、repository、environment のいずれかで指定してください');
      const gates = {};
      for (const gate of GATES) {
        const value = task.gates?.[gate];
        if (value !== undefined && (!value || typeof value !== 'object' || Array.isArray(value))) throw Error(`${gate} gate は object で指定してください`);
        const items = evidence(value?.evidence, taskView, now, counts, gate);
        const warnings = items.flatMap((item) => item.warnings);
        if (value?.status === 'pass') {
          if (!items.length) warnings.push('成功の申告に証跡がありません');
          for (const item of items) {
            for (const [label, field] of [['ref', item.ref], ['result', item.result], ['observed_at', item.observedAt], ['revision', item.revision], ['environment', item.environment]]) {
              if (field === missing) warnings.push(`成功の申告の証跡に ${label} がありません`);
            }
            if (item.result !== missing && item.result !== 'pass') warnings.push(`成功の申告の証跡の result が pass ではありません: ${item.result}`);
          }
        }
        gates[gate] = { statusLabel: statusLabel(value?.status), evidence: items, warnings: [...new Set(warnings)] };
      }
      const reviewView = review(task.review, taskView, now);
      if (reviewView.status === 'accepted' && ['fail', 'blocked'].includes(task.gates?.acceptance?.status)) reviewView.warnings.push(`受入の申告と acceptance gate の ${task.gates.acceptance.status} が矛盾します`);
      return {
        ...taskView,
        gates,
        review: reviewView,
      };
    });
    return { ok: true, tasks, coverage: coverage(input.coverage) };
  }

  window.KT_REVIEW_MODEL = { parse, githubUrl, GATES, missing };
})();
