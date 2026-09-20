import React from 'react';
import styles from './styles.module.css';

export default function Emblem({group}) {
  const paths = {
    components: <><path d="m12 3 9 5-9 5-9-5 9-5Z" /><path d="m3 12 9 5 9-5M3 16l9 5 9-5" /></>,
    drivers: <><rect x="6" y="6" width="12" height="12" rx="3" /><path d="M9 3v3m6-3v3M9 18v3m6-3v3M3 9h3m-3 6h3m12-6h3m-3 6h3" /><rect x="10" y="10" width="4" height="4" rx="1" /></>,
    memory: <><rect x="3" y="6" width="18" height="12" rx="2" /><path d="M7 10v4m5-4v4m5-4v4M7 18v3m5-3v3m5-3v3" /></>,
    virtualization: <><path d="m12 3 9 5v9l-9 5-9-5V8l9-5Z" /><path d="m3 8 9 5 9-5m-9 5v9M7.5 5.5l9 5" /></>,
    starry: <path d="m12 2 2.6 7.4L22 12l-7.4 2.6L12 22l-2.6-7.4L2 12l7.4-2.6L12 2Z" />,
    arceos: <><path d="m3 20 9-16 9 16M7 13h10" /><path d="m8 20 4-7 4 7" /></>,
  };
  return <span aria-hidden="true" className={styles.emblem} data-group={group}>
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      {paths[group] || <><path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-12-2 14" /></>}
    </svg>
  </span>;
}

