//! Colour-scheme switch.
//!
//! The scheme is one class on `<html>` (`src/lib/theme.ts`) and every colour in
//! the app is a variable with a value per scheme, so this component owns the
//! choice and nothing else knows about it: no panel re-renders on a switch and
//! no component carries a `dark:` variant of its own layout.

import { useEffect, useState } from 'react'
import { Moon, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { applyTheme, readTheme, type Theme } from '@/lib/theme'

export function ThemeToggle() {
  const [theme, setTheme] = useState<Theme>(readTheme)

  useEffect(() => {
    applyTheme(theme)
  }, [theme])

  const next: Theme = theme === 'dark' ? 'light' : 'dark'

  return (
    <Button
      size="sm"
      variant="ghost"
      onClick={() => setTheme(next)}
      title={next === 'dark' ? '切换到深色' : '切换到浅色'}
      aria-label={next === 'dark' ? '切换到深色' : '切换到浅色'}
    >
      {theme === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
    </Button>
  )
}
