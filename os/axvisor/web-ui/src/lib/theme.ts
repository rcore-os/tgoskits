//! Colour scheme of the dashboard, remembered across reloads.
//!
//! The scheme is a class on `<html>` and nothing else: every colour in the app
//! is a CSS variable (`src/index.css`) with a value per scheme, so switching is
//! one class and no component knows which scheme is on. A terminal-heavy
//! dashboard is read for hours at a time, which is why the dark scheme is the
//! default rather than an option someone has to find.

export type Theme = 'dark' | 'light'

const STORAGE_KEY = 'axvisor.theme'

/** The scheme stored by a previous visit, `dark` on a first one. */
export function readTheme(): Theme {
  const stored = window.localStorage.getItem(STORAGE_KEY)
  return stored === 'light' ? 'light' : 'dark'
}

/** Applies `theme` to the document and remembers it for the next visit. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement
  root.classList.toggle('dark', theme === 'dark')
  // Native controls — scrollbars, the terminal's own selection colours — follow
  // this rather than the class, and would otherwise stay light under dark.
  root.style.colorScheme = theme
  window.localStorage.setItem(STORAGE_KEY, theme)
}
