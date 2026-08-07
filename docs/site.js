(function () {
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
