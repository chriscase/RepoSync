/* RepoSync #263 prototypes: theme switch, "More" control and option switch. No network, no dependencies.
   Theme: dark unless chosen. Resolution: address hash (#light, #dark, optionally with -nobar) > saved choice > dark.
   Adding -menu to the hash (#light-menu, #dark-nobar-menu) opens the "More" control on load, for captures. */
(function () {
  var root = document.documentElement;
  var shells = document.querySelectorAll('.rs-layout-shell');
  var KEY = 'rs263.theme', theme = 'dark';
  function apply() {
    root.setAttribute('data-rs-theme', theme);
    for (var i = 0; i < shells.length; i++) shells[i].setAttribute('data-rs-theme', theme);
    var buttons = document.querySelectorAll('[data-theme-set]');
    for (var j = 0; j < buttons.length; j++) buttons[j].setAttribute('aria-pressed', String(buttons[j].getAttribute('data-theme-set') === theme));
    var select = document.querySelector('[data-theme-select]');
    if (select) select.value = theme;
  }
  function read() {
    var parts = (location.hash || '').replace('#', '').split('-');
    if (parts.indexOf('dark') >= 0) return 'dark';
    if (parts.indexOf('light') >= 0) return 'light';
    try { if (localStorage.getItem(KEY) === 'light') return 'light'; } catch (e) { /* storage unavailable: dark */ }
    return 'dark';
  }
  function set(next, persist) {
    theme = next === 'light' ? 'light' : 'dark'; apply();
    if (!persist) return;
    try { localStorage.setItem(KEY, theme); } catch (e) { /* applies for this page only */ }
  }
  function bar() {
    var el = document.querySelector('[data-proto-bar]');
    if (el) el.hidden = (location.hash || '').indexOf('nobar') >= 0;
  }
  function more(shell, open, focus) {
    var button = shell.querySelector('.shell-menu');
    if (open) shell.setAttribute('data-nav-open', ''); else shell.removeAttribute('data-nav-open');
    if (button) button.setAttribute('aria-expanded', String(open));
    if (!focus) return;
    var first = open ? shell.querySelector('#nav-account a') : button;
    if (first) first.focus();
  }
  function menuFromHash() {
    var open = (location.hash || '').replace('#', '').split('-').indexOf('menu') >= 0;
    for (var i = 0; i < shells.length; i++) if (shells[i].querySelector('.shell-menu')) more(shells[i], open, false);
  }
  set(read(), false); bar(); menuFromHash();
  document.addEventListener('click', function (event) {
    var themed = event.target.closest('[data-theme-set]');
    if (themed) { set(themed.getAttribute('data-theme-set'), true); return; }
    var button = event.target.closest('.shell-menu');
    var shell = button && button.closest('.rs-layout-shell');
    if (shell) more(shell, !shell.hasAttribute('data-nav-open'), true);
  });
  document.addEventListener('keydown', function (event) {
    if (event.key !== 'Escape') return;
    var shell = document.querySelector('.rs-layout-shell[data-nav-open]');
    if (shell) more(shell, false, true);
  });
  document.addEventListener('change', function (event) {
    var el = event.target;
    if (el.matches('[data-theme-select]')) set(el.value, true);
    if (el.matches('[data-layout-select]')) {
      var href = el.options[el.selectedIndex].getAttribute('data-href');
      if (href) location.href = href + location.hash;
    }
  });
  document.addEventListener('submit', function (event) { event.preventDefault(); });
  window.addEventListener('hashchange', function () { set(read(), false); bar(); menuFromHash(); });
})();
