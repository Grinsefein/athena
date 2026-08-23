// Animates an element's height whenever its content size changes, so the
// card grows/shrinks smoothly instead of jumping (URL -> results etc.).
//
// IMPORTANT: We observe the element's CONTENT (first child), never the
// animated node itself. Animating the node's height would re-trigger a
// self-referential observer on every frame, restarting the animation from
// the current mid-flight height each time — the classic "stuttering
// resize". The child's layout is unaffected by the parent's clipped height.
export function smoothHeight(node, { duration = 380, disabled = false } = {}) {
  let prev = node.getBoundingClientRect().height;
  let current = null;

  const prefersReduced = () =>
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Outer borders belong to the node's box but not to the content child.
  const cs = getComputedStyle(node);
  const borders =
    parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);

  const content = node.firstElementChild || node;
  const measure = () => content.getBoundingClientRect().height + borders;

  const observer = new ResizeObserver(() => {
    const next = measure();
    if (Math.abs(next - prev) < 1) return;
    if (disabled || prefersReduced()) {
      prev = next;
      return;
    }
    // If an animation is already running, continue from wherever it
    // currently renders instead of snapping back to the previous target.
    const from = current ? node.getBoundingClientRect().height : prev;
    if (current) current.cancel();
    node.style.overflow = 'hidden';
    const anim = node.animate(
      [{ height: `${from}px` }, { height: `${next}px` }],
      { duration, easing: 'cubic-bezier(0.22, 1, 0.36, 1)' }
    );
    current = anim;
    anim.finished
      .catch(() => {})
      .finally(() => {
        // A newer animation may have replaced this one meanwhile.
        if (current === anim) {
          current = null;
          node.style.overflow = '';
        }
      });
    prev = next;
  });

  observer.observe(content);
  return {
    update(opts = {}) {
      disabled = !!opts.disabled;
      duration = opts.duration ?? duration;
    },
    destroy() {
      observer.disconnect();
      if (current) current.cancel();
      node.style.overflow = '';
    }
  };
}
