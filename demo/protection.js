// Deterministic teaching simulation. This page never reads or writes the real mount.
(() => {
  const get = id => document.getElementById(id);
  const clone = value => JSON.parse(JSON.stringify(value));
  let state, saved, enabled = false, dirty = false, selected = null, successes = 0;
  function reset() {
    state = { files: {}, trash: [], snapshot: null, nextId: 1 };
    saved = clone(state); enabled = false; dirty = false; selected = null; successes = 0;
  }
  function sync() { saved = clone(state); dirty = false; }
  function archive(path, reason) {
    if (state.trash.length >= 1024) throw Error('回收站已满，原文件保留，请先清理。');
    const content = state.files[path];
    const bytes = state.trash.reduce((n, r) => n + new TextEncoder().encode(r.content).length, 0);
    if (bytes + new TextEncoder().encode(content).length > 64 * 1024 * 1024) throw Error('回收站容量不足，操作已拒绝。');
    const record = { id: state.nextId++, path, content, reason };
    state.trash.push(record); selected = record.id;
  }
  function render(message) {
    get('protection-live').textContent = JSON.stringify(state.files, null, 2);
    get('protection-saved').textContent = JSON.stringify(saved.files, null, 2);
    get('protection-state').textContent = 'dirty = ' + dirty + ' · 自动同步：' + (enabled ? '30 秒' : '关闭') + ' · 自动成功：' + successes;
    get('protection-auto').textContent = enabled ? '关闭自动同步' : '开启自动同步';
    get('protection-auto').setAttribute('aria-pressed', String(enabled));
    const list = get('protection-trash');
    list.replaceChildren();
    if (!state.trash.length) list.textContent = '回收站为空。记录上限 1,024 条 / 64 MiB，满时拒绝删除。';
    for (const r of state.trash) {
      const button = document.createElement('button');
      button.className = 'button'; button.type = 'button';
      button.textContent = '#' + r.id + ' ' + r.path + ' · ' + r.reason;
      button.setAttribute('aria-pressed', String(selected === r.id));
      button.addEventListener('click', () => { selected = r.id; render('已选择 #' + r.id + '，原内容：' + r.content); });
      list.append(button);
    }
    if (message) get('protection-result').textContent = message;
  }
  document.querySelectorAll('[data-protection]').forEach(button => button.addEventListener('click', () => {
    let message = '';
    try {
      switch (button.dataset.protection) {
        case 'write':
          state.files['report.txt'] = '报告初稿'; dirty = true;
          message = '写入了报告。先开启自动同步，再推进时间，观察保存结果。'; break;
        case 'auto':
          enabled = !enabled;
          message = enabled ? '已模拟以 --auto-sync 30 挂载；点击“推进 30 秒”触发一次检查。' : '自动同步已关闭。'; break;
        case 'tick':
          if (enabled && dirty) { sync(); successes++; message = '自动同步成功：当前文件、回收站和快照一起保存。'; }
          else message = enabled ? '没有未同步修改，跳过本次保存。' : '自动同步未开启，推进时间不会保存。';
          break;
        case 'sync': sync(); message = '手动同步成功。'; break;
        case 'snapshot':
          if (state.snapshot !== null) throw Error('before-edit 已存在，拒绝重复创建。');
          state.snapshot = clone(state.files); sync(); message = '已建立 before-edit 快照并保存当前状态。'; break;
        case 'edit':
          if (!('report.txt' in state.files)) throw Error('请先写入报告。');
          state.files['report.txt'] = '误改内容'; dirty = true;
          message = '直接覆盖写入不进入回收站；可与快照比较。'; break;
        case 'replace':
          if (!('report.txt' in state.files)) throw Error('请先写入报告。');
          archive('report.txt', '改名覆盖'); state.files['report.txt'] = '替换后的报告'; dirty = true;
          message = '模拟临时文件改名替换报告；旧目标内容进入回收站。'; break;
        case 'note':
          state.files['note.txt'] = '没有建立快照的新笔记'; dirty = true; message = '新增 note.txt。'; break;
        case 'delete':
          if (!('note.txt' in state.files)) throw Error('请先新增笔记。');
          archive('note.txt', '删除'); delete state.files['note.txt']; dirty = true;
          message = '原路径已消失；删除时刻的内容保留在回收站，尚需同步。'; break;
        case 'restore': {
          const r = state.trash.find(r => r.id === selected);
          if (!r) throw Error('请先删除或替换文件，再选择回收记录。');
          const target = 'recovered-' + r.id + '.txt';
          if (target in state.files) throw Error('目标已存在：恢复拒绝覆盖。');
          state.files[target] = r.content; dirty = true;
          message = '恢复到 ' + target + '；原回收记录保留，结果尚需同步。'; break;
        }
        case 'purge':
          if (!state.trash.some(r => r.id === selected)) throw Error('请先选择回收记录。');
          state.trash = state.trash.filter(r => r.id !== selected); selected = null; sync();
          message = '已彻底清理所选记录，并同步全部当前状态。'; break;
        case 'diff': {
          if (state.snapshot === null) throw Error('请先建立 before-edit 快照。');
          const changes = [];
          for (const path of [...new Set([...Object.keys(state.snapshot), ...Object.keys(state.files)])].sort()) {
            if (!(path in state.snapshot)) changes.push('新增  ' + path);
            else if (!(path in state.files)) changes.push('已删除  ' + path);
            else if (state.files[path] !== state.snapshot[path]) changes.push('内容修改  ' + path);
          }
          get('protection-diff').textContent = changes.join('\n') || '没有变化';
          message = '比较完成。本页演示内容与路径差异；真实命令还比较权限、所有者及修改时间。'; break;
        }
        case 'crash':
          state = clone(saved); dirty = false; selected = null; successes = 0;
          message = '已模拟异常退出和同参数重挂载：加载最近成功提交，计数器重置。'; break;
        case 'reset':
          reset(); get('protection-diff').textContent = '建立快照、修改文件后，点击“查看差异”。'; message = '实验已重置。'; break;
      }
    } catch (error) { message = error.message; }
    render(message);
  }));
  reset(); render();
})();
