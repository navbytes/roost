(function () {
  // Demo-mock working glyphs cycle pi's real spinner frames at the released
  // 80ms cadence. Under prefers-reduced-motion the markup's static ⠋ stands
  // (the chrome's own sanctioned steady frame) — the interval never starts.
  var frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
  var spinners = document.querySelectorAll('.spin-glyph');
  if (spinners.length && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    var fi = 0;
    setInterval(function () {
      fi = (fi + 1) % frames.length;
      spinners.forEach(function (el) { el.textContent = frames[fi]; });
    }, 80);
  }

  document.querySelectorAll('.js-copy').forEach(function (btn) {
    var original = btn.textContent;
    var ct;
    var show = function (label) {
      btn.textContent = label;
      clearTimeout(ct);
      ct = setTimeout(function () { btn.textContent = original; }, 1400);
    };
    btn.addEventListener('click', function () {
      var text = btn.getAttribute('data-copy-text') || '';
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(function () { show('Copied ✓'); }, function () { show('Select + ⌘C'); });
      } else {
        show('Select + ⌘C');
      }
    });
  });
})();
