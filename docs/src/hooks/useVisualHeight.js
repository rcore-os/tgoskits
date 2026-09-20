import {useEffect, useRef} from 'react';

// Match illustration sizing to natural copy height without stretching text or SVGs.
export default function useVisualHeight() {
  const root = useRef(null);
  useEffect(() => {
    if (!root.current) return undefined;
    const observers = [...root.current.querySelectorAll('[data-visual-pair]')].map(pair => {
      const copy = pair.querySelector('[data-visual-copy]');
      if (!copy) return null;
      const update = () => pair.style.setProperty('--visual-copy-height', `${copy.offsetHeight}px`);
      const observer = new ResizeObserver(update);
      observer.observe(copy);
      update();
      return {pair, observer};
    });
    return () => observers.forEach(item => {
      item?.observer.disconnect();
      item?.pair.style.removeProperty('--visual-copy-height');
    });
  }, []);
  return root;
}
