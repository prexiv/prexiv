// Render LaTeX math anywhere in the page once both katex.js and auto-render.js
// have loaded. Defer-loaded so this file runs after both KaTeX scripts have
// executed; if for some reason renderMathInElement isn't yet available we
// retry briefly.
(function () {
  'use strict';

  // Custom-macro globals. PreXiv manuscripts use a handful of common
  // LaTeX-source `\newcommand`s and `\DeclareMathOperator`s in their
  // bodies; titles and abstracts get rendered by KaTeX without the
  // preamble, so we have to teach the renderer what these mean.
  // Extend liberally — false-positive expansions are harmless because
  // KaTeX only triggers on `\name` followed by non-letter.
  const macros = {
    // Operators (named functions, upright text)
    '\\Var':   '\\operatorname{Var}',
    '\\Cov':   '\\operatorname{Cov}',
    '\\Tr':    '\\operatorname{Tr}',
    '\\tr':    '\\operatorname{tr}',
    '\\xc':    '\\operatorname{xc}',
    '\\diag':  '\\operatorname{diag}',
    '\\rank':  '\\operatorname{rank}',
    '\\supp':  '\\operatorname{supp}',
    '\\sgn':   '\\operatorname{sgn}',
    '\\Re':    '\\operatorname{Re}',
    '\\Im':    '\\operatorname{Im}',
    '\\limsupop': '\\operatorname*{lim\\,sup}',
    '\\liminfop': '\\operatorname*{lim\\,inf}',

    // Number sets / blackboard letters
    '\\R':     '\\mathbb{R}',
    '\\N':     '\\mathbb{N}',
    '\\Z':     '\\mathbb{Z}',
    '\\Q':     '\\mathbb{Q}',
    '\\C':     '\\mathbb{C}',
    '\\E':     '\\mathbb{E}',
    '\\P':     '\\mathbb{P}',

    // Differential element — common in physics body text
    '\\dd':    '\\,\\mathrm{d}',
    '\\diff':  '\\,\\mathrm{d}',

    // Calligraphic shortcuts the math-phys batch uses
    '\\cK':    '\\mathcal{K}',
    '\\cQ':    '\\mathcal{Q}',
    '\\cR':    '\\mathcal{R}',
    '\\cL':    '\\mathcal{L}',
    '\\cN':    '\\mathcal{N}',
    '\\cF':    '\\mathcal{F}',
    '\\cG':    '\\mathcal{G}',
  };

  const opts = {
    delimiters: [
      { left: '$$', right: '$$', display: true  },
      { left: '\\[', right: '\\]', display: true  },
      { left: '$',  right: '$',  display: false },
      { left: '\\(', right: '\\)', display: false },
    ],
    macros: macros,
    throwOnError: false,
    ignoredTags:    ['script', 'noscript', 'style', 'textarea', 'pre', 'code', 'option'],
    ignoredClasses: ['no-katex'],
  };

  function tryRender(root) {
    if (typeof window.renderMathInElement === 'function') {
      try { window.renderMathInElement(root || document.body, opts); }
      catch (e) { console.warn('[katex] render error:', e); }
      return true;
    }
    return false;
  }

  // Initial render: retry briefly if auto-render hasn't installed itself yet.
  let attempts = 0;
  function init() {
    if (tryRender(document.body)) return;
    if (++attempts < 40) setTimeout(init, 50);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }

  // Re-render any subtree the page wants to typeset later (e.g. after AJAX).
  // Usage from page scripts: window.preXivRenderMath(el)
  window.preXivRenderMath = tryRender;
})();
