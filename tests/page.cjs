// 页面验收需要 Playwright；仅用于开发验证，讲解页面本身没有外部依赖。
const { chromium } = require(process.env.TEAMFS_PLAYWRIGHT || 'playwright');
const assert = require('node:assert/strict');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const fs = require('node:fs');

(async () => {
  const root = path.resolve(__dirname, '..');
  const results = [];
  const browser = await chromium.launch({ headless: true, executablePath: process.env.TEAMFS_BROWSER || undefined });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, permissions: ['clipboard-read', 'clipboard-write'] });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(pathToFileURL(path.join(root, 'demo/index.html')).href);
    // 截图时关闭平滑滚动，避免捕获尚未到达目的位置的中间帧。
    await page.addStyleTag({ content: 'html { scroll-behavior: auto !important; }' });
    const ok = message => { results.push('PASS: ' + message); console.log(results.at(-1)); };
    assert.equal(await page.title(), 'TeamFS · 从 Rust 程序到文件系统');
    assert.equal(await page.locator('[data-index]').count(), 10);
    assert.equal(await page.locator('.flow-node.active').count(), 1);
    for (let index = 0; index < 10; index++) {
      await page.locator(`[data-index="${index}"]`).click();
      assert.equal((await page.locator('#sim-command').textContent()).includes('\n'), false);
    }
    await page.locator('#reset').click();
    ok('本地文件首屏正常加载，10 个操作与请求层可用');

    async function recovery(action) { await page.locator(`[data-recovery="${action}"]`).click(); }
    await recovery('write');
    await recovery('crash');
    assert.match(await page.locator('#recovery-current').textContent(), /report.txt: （不存在）/);
    for (const action of ['write', 'snapshot', 'edit', 'restore', 'sync', 'remount']) await recovery(action);
    assert.match(await page.locator('#recovery-current').textContent(), /report-recovered.txt: 报告初稿/);
    assert.equal(await page.locator('#recovery-current').textContent(), await page.locator('#recovery-saved').textContent());
    await recovery('restore');
    assert.match(await page.locator('#recovery-result').textContent(), /拒绝覆盖/);
    await recovery('reset');
    ok('新版保存与恢复：未同步异常退出丢失、快照隔离、恢复、同步与不覆盖');

    async function scenario(index) {
      await page.locator(`[data-index="${index}"]`).click();
      for (let layer = 0; layer < 5; layer++) await page.locator('#next').click();
      assert.match(await page.locator('#position').textContent(), /层 6 \/ 6/);
    }
    await scenario(3);
    await page.locator('[data-path="meeting/a.txt"]').click();
    assert.equal(await page.locator('#preview-content').textContent(), '第一次会议\n');
    await scenario(4);
    assert.equal(await page.locator('#preview-content').textContent(), '第一次会议\n第二条\n');
    await scenario(5);
    assert.equal(await page.locator('#preview-content').textContent(), '短\n');
    assert.match(await page.locator('#preview-meta').textContent(), /4 字节/);
    ok('创建、追加、短内容覆盖与 UTF-8 字节大小一致');

    await scenario(6);
    assert.equal(await page.locator('[data-path="meeting/a.txt"]').count(), 0);
    assert.equal(await page.locator('[data-path="meeting/b.txt"]').count(), 1);
    assert.match(await page.locator('#preview-meta').textContent(), /inode=6/);
    await scenario(7);
    assert.equal(await page.locator('[data-path="meeting/b.txt"]').count(), 0);
    await scenario(8);
    assert.equal(await page.locator('[data-path="meeting"]').count(), 0);
    ok('改名保持 inode，文件和空目录删除同步更新树');

    await page.locator('[data-index="9"]').click();
    assert.equal(await page.locator('[data-path="session.txt"]').count(), 1);
    for (let layer = 0; layer < 5; layer++) await page.locator('#next').click();
    assert.equal(await page.locator('[data-path="session.txt"]').count(), 0);
    assert.match(await page.locator('#preview-content').textContent(), /^欢迎来到 TeamFS/);
    assert.equal(await page.locator('#next').isDisabled(), true);
    await page.locator('#reset').click();
    assert.equal(await page.locator('#previous').isDisabled(), true);
    await page.locator('#next').focus();
    await page.keyboard.press('Enter');
    assert.match(await page.locator('#position').textContent(), /层 2 \/ 6/);
    ok('重挂载与重置恢复初始内容，键盘推进可用');

    await page.locator('[data-copy="start-code"]').click();
    await page.waitForFunction(() => document.querySelector('[data-copy="start-code"]').textContent === '已复制', null, { timeout: 5000 });
    assert.equal(await page.locator('[data-copy="start-code"]').textContent(), '已复制');
    const clipboard = await page.evaluate(() => navigator.clipboard.readText());
    // Windows 剪贴板把换行规范化为 CRLF；比较实际文本内容。
    assert.equal(clipboard.replace(/\r\n/g, '\n'), await page.locator('#start-code').textContent());
    ok('命令复制：实际剪贴板内容保留路径引号与换行');

    fs.mkdirSync(path.join(root, 'artifacts'), { recursive: true });
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({ path: path.join(root, 'artifacts/page-desktop.png'), fullPage: true });
    await page.screenshot({ path: path.join(root, 'artifacts/page-hero.png') });
    await page.locator('#lab').scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(root, 'artifacts/page-lab.png') });
    for (const width of [390, 360]) {
      await page.setViewportSize({ width, height: 844 });
      await page.evaluate(() => window.scrollTo(0, 0));
      const dimensions = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
      assert.ok(dimensions.scroll <= dimensions.width, JSON.stringify(dimensions));
      await page.locator('[data-index="5"]').click();
      await page.locator('#next').click();
    }
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({ path: path.join(root, 'artifacts/page-mobile.png'), fullPage: true });
    await page.locator('#lab').scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(root, 'artifacts/page-mobile-lab.png') });
    ok('390px / 360px 窄窗口无页面横向溢出，操作仍可用');
    assert.deepEqual(errors, []);
    ok('全程没有 JavaScript 运行异常');
    fs.writeFileSync(path.join(root, 'artifacts/page-check.txt'), results.join('\n') + '\n', 'utf8');
    await context.close();
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
