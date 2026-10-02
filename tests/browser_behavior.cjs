// Execute the production JXA scripts against stateful browser objects. No
// osascript process or real browser is used by these checks.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync(path.join(__dirname, '../src/macos/chrome.rs'), 'utf8');
const scripts = Object.fromEntries([...source.matchAll(/const (\w+_SCRIPT): &str = r#"([\s\S]*?)"#;/g)]
  .map((match) => [match[1], match[2]]));

let nextId = 1;
const bounds = { x: 0, y: 0, width: 800, height: 600 };
function makeTab(value) {
  let url = typeof value === 'string' ? value : value.url;
  const tab = { title: () => url };
  Object.defineProperty(tab, 'url', { get: () => () => url, set: (value) => { url = value; } });
  return tab;
}
function makeWindow(urls, frame = bounds, options = {}) {
  const rawTabs = urls.map(makeTab);
  let currentBounds = { ...frame };
  let active = 1;
  const tabs = () => rawTabs;
  rawTabs.forEach((tab, i) => { tabs[i] = tab; });
  tabs.push = (tab) => {
    if (options.rejectTabAdds) throw new Error('browser busy');
    tabs[rawTabs.length] = tab;
    rawTabs.push(tab);
  };
  const id = nextId++;
  const window = { id: () => id, name: () => options.title || 'window', tabs };
  Object.defineProperty(window, 'activeTabIndex', {
    get: () => () => active,
    set: (value) => {
      if (options.rejectActiveTab) throw new Error('active tab rejected');
      active = value;
    },
  });
  Object.defineProperty(window, 'activeTab', { get: () => rawTabs[active - 1] });
  Object.defineProperty(window, 'bounds', {
    get: () => () => options.unreadableBounds ? {} : currentBounds,
    set: (value) => { if (!options.ignoreBounds) currentBounds = value; },
  });
  return window;
}
function makeBrowser(initialWindows, options = {}) {
  const rawWindows = [...initialWindows];
  const windows = () => rawWindows;
  windows.byId = (id) => {
    const window = rawWindows.find((window) => window.id() === id);
    if (!window) throw new Error('window disappeared');
    return window;
  };
  windows.push = (window) => {
    rawWindows.push(window);
    if (options.concurrentWindow) rawWindows.push(makeWindow(['https://user.example']));
    if (options.throwAfterCreate) throw new Error('creation response lost');
  };
  return { windows, running: () => true, activate() {}, Tab: makeTab,
    Window: () => makeWindow(['chrome://newtab'], bounds, options.createdWindow || {}) };
}
function run(name, browser, spec) {
  assert.ok(scripts[name], `missing production script ${name}`);
  return vm.runInNewContext(`${scripts[name]}\nrun(['com.google.Chrome', payload]);`, {
    Application: () => browser, payload: JSON.stringify(spec), delay() {},
  });
}
function restore(browser, specs) {
  return JSON.parse(run('RESTORE_SCRIPT', browser, specs));
}
function moveSpec(id) {
  return { windowId: id, fx: 0, fy: 0, fw: 800, fh: 600,
    tx: 900, ty: 0, tw: 800, th: 600 };
}
const tests = [
  ['restore preserves an existing blank window', () => {
    const kept = makeWindow(['chrome://newtab']);
    const browser = makeBrowser([kept]);
    const result = restore(browser, [{ ...bounds, x: 900, urls: ['https://docs.example'], active: 1 }]);
    assert.equal(result.errors.length, 0);
    assert.equal(browser.windows().length, 2);
    assert.equal(kept.activeTab.url(), 'chrome://newtab');
    assert.equal(kept.bounds().x, 0);
  }],
  ['browser bounds use the selected ID when geometry overlaps', () => {
    const first = makeWindow(['https://first.example']);
    const selected = makeWindow(['https://selected.example']);
    const browser = makeBrowser([first, selected]);
    assert.equal(run('SET_BOUNDS_SCRIPT', browser, moveSpec(selected.id())), 'ok');
    assert.equal(selected.bounds().x, 900);
    assert.equal(first.bounds().x, 0);
  }],
  ['missing browser identity never falls back to geometry', () => {
    const kept = makeWindow(['https://kept.example']);
    const result = run('SET_BOUNDS_SCRIPT', makeBrowser([kept]), moveSpec(99999));
    assert.match(result, /^error:/);
    assert.equal(kept.bounds().x, 0);
  }],
  ['reconciliation uses the same ID after bounds change', () => {
    const first = makeWindow(['https://shared.example']);
    const selected = makeWindow(['https://shared.example']);
    const result = run('RECONCILE_SCRIPT', makeBrowser([first, selected]), {
      ...bounds, windowId: selected.id(), urls: ['https://shared.example', 'https://missing.example'],
      activeUrl: 'https://missing.example',
    });
    assert.equal(result, 'added 1');
    assert.equal(first.tabs().length, 1);
    assert.equal(selected.tabs().length, 2);
    assert.equal(selected.activeTab.url(), 'https://missing.example');
  }],
  ['failed tab creation is reported', () => {
    const window = makeWindow(['https://kept.example'], bounds, { rejectTabAdds: true });
    const result = run('RECONCILE_SCRIPT', makeBrowser([window]), {
      ...bounds, windowId: window.id(), urls: ['https://kept.example', 'https://missing.example'],
    });
    assert.match(result, /^error:/);
    assert.equal(window.tabs().length, 1);
  }],
  ['initial identity resolution rejects overlapping untitled candidates', () => {
    const browser = makeBrowser([makeWindow(['one']), makeWindow(['two'])]);
    assert.equal(JSON.parse(run('RESOLVE_WINDOW_SCRIPT', browser, { ...bounds, title: null })), null);
  }],
  ['initial identity resolution uses observed title and frame', () => {
    const first = makeWindow(['one'], bounds, { title: 'first' });
    const selected = makeWindow(['two'], bounds, { title: 'selected' });
    assert.equal(JSON.parse(run('RESOLVE_WINDOW_SCRIPT', makeBrowser([first, selected]), {
      ...bounds, title: 'selected',
    })), selected.id());
  }],
  ['two live observations cannot claim one scripting ID', () => {
    const browser = makeBrowser([makeWindow(['one'], bounds, { title: 'selected' })]);
    const result = run('RESOLVE_WINDOW_SCRIPT', browser, [
      { ...bounds, title: 'selected' }, { ...bounds, x: 1, title: 'selected' },
    ]);
    assert.deepEqual(JSON.parse(result), [null, null]);
  }],
  ['window movement retains identity after its source bounds change', () => {
    const kept = makeWindow(['kept']);
    const selected = makeWindow(['selected']);
    selected.bounds = { ...bounds, x: 2000 };
    assert.equal(run('SET_BOUNDS_SCRIPT', makeBrowser([kept, selected]), moveSpec(selected.id())), 'ok');
    assert.equal(selected.bounds().x, 900);
    assert.equal(kept.bounds().x, 0);
  }],
  ['unobserved bounds writes do not report success', () => {
    const window = makeWindow(['one'], bounds, { ignoreBounds: true });
    assert.match(run('SET_BOUNDS_SCRIPT', makeBrowser([window]), moveSpec(window.id())), /^error:/);
  }],
  ['missing reconciliation identity never adds tabs to another window', () => {
    const window = makeWindow(['https://shared.example']);
    assert.match(run('RECONCILE_SCRIPT', makeBrowser([window]), {
      ...bounds, windowId: 99999, urls: ['https://shared.example', 'https://missing.example'],
    }), /^error:/);
    assert.equal(window.tabs().length, 1);
  }],
  ['unreadable bounds do not report success', () => {
    const window = makeWindow(['one'], bounds, { unreadableBounds: true });
    assert.match(run('SET_BOUNDS_SCRIPT', makeBrowser([window]), moveSpec(window.id())), /^error:/);
  }],
  ['browser mutations require an explicit scripting identity', () => {
    const window = makeWindow(['https://kept.example']);
    const browser = makeBrowser([window]);
    assert.match(run('SET_BOUNDS_SCRIPT', browser, moveSpec(undefined)), /^error:/);
    assert.match(run('RECONCILE_SCRIPT', browser, { urls: ['https://missing.example'] }), /^error:/);
    assert.equal(window.bounds().x, 0);
    assert.equal(window.tabs().length, 1);
  }],
  ['active tab failures are reported', () => {
    const window = makeWindow(['https://shared.example'], bounds, { rejectActiveTab: true });
    assert.match(run('RECONCILE_SCRIPT', makeBrowser([window]), {
      windowId: window.id(), urls: ['https://shared.example'], activeUrl: 'https://shared.example',
    }), /^error:/);
  }],
  ['creation preserves all existing windows and returns new identities', () => {
    const blank = makeWindow(['chrome://newtab']);
    const kept = makeWindow(['https://kept.example']);
    const browser = makeBrowser([blank, kept]);
    const result = restore(browser, [
      { ...bounds, x: 900, urls: ['https://one.example', 'https://two.example'], active: 2 },
      { ...bounds, x: 1800, urls: ['https://three.example'], active: 1 },
    ]);
    assert.equal(result.errors.length, 0);
    assert.equal(result.windows.length, 2);
    assert.equal(new Set(result.windows.map((window) => window.id)).size, 2);
    assert.ok(result.windows.every((window) => window.id !== blank.id() && window.id !== kept.id()));
    assert.equal(browser.windows().length, 4);
    assert.equal(blank.activeTab.url(), 'chrome://newtab');
    assert.equal(kept.activeTab.url(), 'https://kept.example');
    assert.equal(browser.windows()[2].activeTab.url(), 'https://two.example');
  }],
  ['concurrent creation rejects ambiguous ownership before tab mutations', () => {
    const browser = makeBrowser([], { concurrentWindow: true });
    const result = restore(browser, [{ ...bounds, urls: ['https://saved.example'], active: 1 }]);
    assert.match(result.errors[0], /ambiguous/);
    assert.equal(result.windows[0], null);
    assert.equal(browser.windows()[0].activeTab.url(), 'chrome://newtab');
    assert.equal(browser.windows()[1].activeTab.url(), 'https://user.example');
  }],
  ['a lost creation response can still resolve the single new window', () => {
    const result = restore(makeBrowser([], { throwAfterCreate: true }), [
      { ...bounds, urls: ['https://saved.example'], active: 1 },
    ]);
    assert.equal(result.errors.length, 0);
    assert.ok(result.windows[0]);
  }],
  ['creation reports partial tab and bounds failures', () => {
    for (const createdWindow of [{ rejectTabAdds: true }, { ignoreBounds: true }, { rejectActiveTab: true }, { unreadableBounds: true }]) {
      const result = restore(makeBrowser([], { createdWindow }), [
        { ...bounds, x: 900, urls: ['https://one.example', 'https://two.example'], active: 2 },
      ]);
      assert.equal(result.errors.length, 1);
      assert.equal(result.windows[0], null);
    }
  }],
];
let failures = 0;
for (const [name, check] of tests) {
  try { check(); console.log(`PASS ${name}`); }
  catch (error) { failures++; console.error(`FAIL ${name}: ${error.message}`); }
}
console.log(`${tests.length - failures}/${tests.length} browser checks passed`);
process.exitCode = failures ? 1 : 0;
