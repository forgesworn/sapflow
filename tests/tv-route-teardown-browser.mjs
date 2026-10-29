// Generated loopback response only. Exercise a real pending Playwright route.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {pathToFileURL} from 'node:url';
import {closeRoutedBrowser} from './tv-browser-cleanup.mjs';

const {chromium} = await import(pathToFileURL(process.env.SAPFLOW_PLAYWRIGHT_MODULE).href);
const server = createServer((request, response) => response.end('Generated fixture'));
let browser, release, timer;
try {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  browser = await chromium.launch({channel: 'chrome', headless: true,
    args: ['--disable-background-networking']});
  const context = await browser.newContext({serviceWorkers: 'block'});
  const entered = Promise.withResolvers(), finished = Promise.withResolvers();
  release = Promise.withResolvers();
  await context.route(`http://127.0.0.1:${server.address().port}/delayed`, async route => {
    entered.resolve();
    try {
      await release.promise;
      // Unrouting/closing can handle this request before the callback resumes.
      await route.fulfill({body: 'Generated delayed response'});
    } finally {
      finished.resolve();
    }
  });
  const page = await context.newPage();
  const navigation = page.goto(`http://127.0.0.1:${server.address().port}/delayed`).catch(() => {});
  await entered.promise;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error('Cleanup waited for a pending route callback')), 5000);
  });
  await Promise.race([closeRoutedBrowser(browser, context), deadline]);
  clearTimeout(timer);
  assert.equal(browser.isConnected(), false);
  release.resolve();
  await finished.promise;
  await navigation;
  // Give any unhandled callback rejection a turn to fail this process.
  await new Promise(resolve => setImmediate(resolve));
  console.log('Pending route callback settled safely after browser cleanup');
} finally {
  clearTimeout(timer);
  release?.resolve();
  await browser?.close();
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
