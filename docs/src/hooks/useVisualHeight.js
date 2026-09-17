import {useEffect} from 'react';

// Match illustration sizing to natural copy height without stretching text or SVGs.
export default function useVisualHeight() {
  useEffect(() => {
    const observers = [...document.querySelectorAll('[data-visual-pair]')].map(pair => {
      const copy = pair.querySelector('[data-visual-copy]');
      if (!copy) return null;
      const update = () => pair.style.setProperty('--visual-copy-height', `${copy.getBoundingClientRect().height}px`);
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
}
