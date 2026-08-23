// Animates an element's height whenever its content size changes, so the
// card grows/shrinks smoothly instead of jumping (URL -> results etc.).
export function smoothHeight(node, { duration = 380, disabled = false } = {}) {
  let prev = node.getBoundingClientRect().height;
  let raf = 0;

  const prefersReduced = () =>
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  const observer = new ResizeObserver(() => {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => {
      const next = node.getBoundingClientRect().height;
      if (Math.abs(next - prev) < 1) return;
      if (disabled || prefersReduced()) {
        prev = next;
        return;
      }
      const from = prev;
      const to = next;
      node.style.overflow = 'hidden';
      const animation = node.animate(
        [{ height: `${from}px` }, { height: `${to}px` }],
        { duration, easing: 'cubic-bezier(0.22, 1, 0.36, 1)' }
      );
      animation.finished
        .catch(() => {})
        .finally(() => {
          node.style.overflow = '';
        });
      prev = next;
    });
  });

  observer.observe(node);
  return {
    update(opts = {}) {
      disabled = !!opts.disabled;
      duration = opts.duration ?? duration;
    },
    destroy() {
      cancelAnimationFrame(raf);
      observer.disconnect();
      node.style.overflow = '';
    }
  };
}
