(() => {
  const $=id=>document.getElementById(id);
  let events=[],session='',cursor=0,previous=null,paused=false,selected=null;
  const labels={open:'打开文件',flush:'关闭前检查',release:'释放句柄',opendir:'打开目录',readdir:'读取目录',releasedir:'释放目录',fsync:'文件同步',fsyncdir:'目录同步',symlink:'创建链接',readlink:'读取链接',mknod:'创建节点',invalid_control_request:'无效管理请求',create:'创建文件',mkdir:'创建目录',read:'读取',write:'写入',rename:'改名 / 移动',unlink:'删除文件',rmdir:'删除目录',setattr:'修改属性',
    commit:'同步提交',sync:'手动同步',snapshot_create:'创建快照',snapshot_delete:'删除快照',restore:'恢复文件',restore_tree:'恢复目录',
    trash_restore:'回收恢复',trash_purge:'清理记录',trash_purge_all:'清空回收站',mount:'挂载',unmount:'卸载',lookup:'查找路径'};
  const errors={2:'路径不存在',5:'读写错误',13:'权限不足',17:'目标已存在',20:'不是目录',22:'参数无效',27:'文件过大',28:'空间不足',30:'只读区域',39:'目录非空'};
  function bytes(n){if(n==null)return '—';for(const unit of ['B','KiB','MiB','GiB']){if(n<1024||unit==='GiB')return n.toFixed(unit==='B'?0:1)+' '+unit;n/=1024;}}
  function clock(ms){return ms?new Date(ms).toLocaleTimeString('zh-CN',{hour12:false}):'—';}
  function filtered(){const path=$('path-filter').value,op=$('operation-filter').value,result=$('result-filter').value,pid=$('pid-filter').value.trim(),duration=Number($('duration-filter').value)||0;
    return events.filter(e=>e.duration_us>=duration*1000&&(!path||(e.path_display||'').includes(path)||(e.destination_display||'').includes(path))&&(!op||e.operation===op)&&(!result||e.result===result)&&(!pid||String(e.caller?.pid)===pid));}
  function renderEvents(){
    const old=$('operation-filter').value;
    const options=[...new Set(events.map(e=>e.operation))].sort();
    $('operation-filter').replaceChildren(new Option('全部操作',''),...options.map(op=>new Option(labels[op]||op,op)));
    $('operation-filter').value=options.includes(old)?old:'';
    const rows=filtered(),body=$('events');body.replaceChildren();
    for(const e of rows.slice().reverse()){
      const tr=document.createElement('tr');tr.tabIndex=0;if(e.seq===selected)tr.className='selected';
      const cells=[clock(e.timestamp_unix_ms),labels[e.operation]||e.operation,
        (e.path_display||'—')+(e.destination_display?' → '+e.destination_display:''),e.caller?.pid??'内部',
        e.actual_bytes==null?'—':bytes(e.actual_bytes),e.duration_us<1000?e.duration_us+' µs':(e.duration_us/1000).toFixed(2)+' ms',
        e.result==='ok'?'成功':(errors[e.errno]||'未成功')+' · '+e.errno];
      cells.forEach((text,index)=>{const td=document.createElement('td');td.textContent=String(text);
        if(index===0){const small=document.createElement('small');small.textContent='#'+e.seq;td.append(small);}
        if(index===6)td.className=e.result==='ok'?'status-ok':'status-error';tr.append(td);});
      const choose=()=>{selected=e.seq;$('detail').textContent=JSON.stringify(e,null,2);renderEvents();};
      tr.addEventListener('click',choose);tr.addEventListener('keydown',key=>{if(key.key==='Enter')choose();});body.append(tr);
    }
    if(!rows.length){const tr=document.createElement('tr'),td=document.createElement('td');td.colSpan=7;td.className='empty';td.textContent='当前筛选条件下没有记录。';tr.append(td);body.append(tr);}
    $('event-count').textContent='显示 '+rows.length+' / '+events.length+' 条最近记录';
  }
  function offline(message,time){
    document.body.classList.add('stale');$('connection').className='connection offline';$('connection').textContent='连接断开';
    $('notice').hidden=false;$('notice').textContent=message+'\n指标和记录保留为最后一次成功采样，不代表当前状态。';
    if(time)$('sample-time').textContent='最后成功采样 '+clock(time);
    $('read-rate').textContent=$('write-rate').textContent='—';previous=null;
  }
  function show(value){
    if(!value.connected){offline(value.error,value.last_good_at_ms);return;}
    document.body.classList.remove('stale');$('connection').className='connection';$('connection').textContent='已连接 · 每秒采样';
    const s=value.status;
    if(value.reset||session!==value.stream.session_id){events=[];selected=null;previous=null;$('detail').textContent='已连接新的挂载会话。';}
    session=value.stream.session_id;cursor=value.cursor;
    const seen=new Set(events.map(e=>e.seq));events.push(...value.events.filter(e=>!seen.has(e.seq)));events=events.slice(-500);
    $('mountpoint').textContent=value.mountpoint+' · '+s.mode+' · v'+s.version;
    $('sample-time').textContent='采样时间 '+clock(value.sampled_at_ms);$('session').textContent='会话 '+session;
    $('sync-state').textContent=s.last_sync_error?'同步失败':s.dirty?'有未同步修改':'已同步';
    $('last-sync').textContent='最近成功 '+clock(s.last_sync_unix*1000);
    $('used').textContent=bytes(s.used_bytes);$('files').textContent=s.files+' 个文件 / '+s.directories+' 个目录';
    const seconds=previous?(value.sampled_at_ms-previous.time)/1000:0;
    $('read-rate').textContent=seconds>0?bytes(Math.max(0,s.read_bytes-previous.read)/seconds):'—';
    $('write-rate').textContent=seconds>0?bytes(Math.max(0,s.write_bytes-previous.write)/seconds):'—';
    if(seconds!==0||!previous)previous={time:value.sampled_at_ms,read:s.read_bytes,write:s.write_bytes};
    $('read-total').textContent='累计 '+bytes(s.read_bytes)+' / '+s.read_calls+' 次';
    $('write-total').textContent='累计 '+bytes(s.write_bytes)+' / '+s.write_calls+' 次';
    $('cache').textContent=bytes(s.cache_bytes);$('unsaved').textContent='未落盘内容驻留 '+bytes(s.resident_unsaved_content_bytes);
    $('history').textContent=s.snapshot_count+' 快照 · '+s.trash_count+' 回收';
    $('history-size').textContent='内容 '+bytes(s.snapshot_bytes+s.trash_bytes);
    const rows=$('perf-rows');rows.replaceChildren();
    const us=n=>n<1000?n+' µs':(n/1000).toFixed(2)+' ms';
    for(const op of s.performance?.operations||[]){
      const tr=document.createElement('tr');
      for(const value of [op.operation,op.calls+' / '+op.errors,us(op.p50_us),us(op.p95_us),us(op.max_us),us(op.lock_wait_total_us),us(op.commit_total_us)]){
        const td=document.createElement('td');td.textContent=value;tr.append(td);
      }rows.append(tr);
    }
    if(!rows.children.length){const tr=document.createElement('tr'),td=document.createElement('td');td.colSpan=7;td.textContent='暂无指标；需要 TeamFS 0.6 挂载。';tr.append(td);rows.append(tr);}
    $('runtime-note').textContent=s.runtime?'打开句柄 '+s.runtime.open_handles+' · 无路径节点 '+(s.runtime.live?.unlinked_nodes??'—')+' · 等待释放的历史 '+(s.runtime.retired_snapshots+s.runtime.retired_trash):'运行时引用指标不可用';
    const file=s.audit.file;
    $('log-status').textContent=file?'文件已写 '+file.written+' 条，未写入 '+file.dropped+' 条':'仅当前挂载的最近记录';
    $('window-note').textContent='本次挂载共记录 '+s.audit.last_seq+' 项，较早 '+s.audit.evicted+' 项已移出内存窗口。页面保留最近 500 项；文件日志另行保存。';
    const warnings=[];
    if(s.last_sync_error)warnings.push('同步错误：'+s.last_sync_error);
    if(file?.last_error)warnings.push('日志文件异常（业务操作继续）：'+file.last_error);
    if(value.gap)warnings.push('本次连接有 '+value.gap+' 条较早记录已移出最近窗口，可查看文件日志。');
    $('notice').hidden=!warnings.length;$('notice').textContent=warnings.join('\n');
    renderEvents();
  }
  async function poll(){
    if(!paused){
      try {const response=await fetch('/api/monitor?after='+cursor+'&session='+encodeURIComponent(session),{cache:'no-store',signal:AbortSignal.timeout(5000)});
        if(!response.ok)throw Error('监控服务返回 HTTP '+response.status);const value=await response.json();if(!paused)show(value);}
      catch(error){if(!paused)offline('无法连接监控服务：'+error.message);}
    }
    setTimeout(poll,1000);
  }
  for(const id of ['path-filter','operation-filter','result-filter','pid-filter','duration-filter'])$(id).addEventListener('input',renderEvents);
  $('pause').addEventListener('click',()=>{paused=!paused;$('pause').textContent=paused?'继续刷新':'暂停刷新';
    if(paused){$('connection').textContent='已暂停 · 显示旧采样';$('connection').className='connection waiting';previous=null;}});
  $('export').addEventListener('click',()=>{const blob=new Blob([JSON.stringify({session_id:session,events:filtered()},null,2)],{type:'application/json'});
    const link=document.createElement('a');link.href=URL.createObjectURL(blob);link.download='teamfs-events.json';link.click();URL.revokeObjectURL(link.href);});
  poll();
})();
