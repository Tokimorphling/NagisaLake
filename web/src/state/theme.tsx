import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'

type Theme = 'system' | 'dark' | 'light'
const STORAGE_KEY = 'nagisalake.theme'

interface ThemeContextValue {
  theme: Theme
  setTheme: (theme: Theme) => void
  /** The theme actually painted, after resolving "system". */
  resolved: 'dark' | 'light'
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

function readStored(): Theme {
  const value = localStorage.getItem(STORAGE_KEY)
  return value === 'dark' || value === 'light' ? value : 'system'
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(readStored)
  const [systemDark, setSystemDark] = useState(
    () => !window.matchMedia('(prefers-color-scheme: light)').matches,
  )

  useEffect(() => {
    const query = window.matchMedia('(prefers-color-scheme: light)')
    const listener = (event: MediaQueryListEvent) => setSystemDark(!event.matches)
    query.addEventListener('change', listener)
    return () => query.removeEventListener('change', listener)
  }, [])

  // "system" leaves the attribute off so the CSS media query decides.
  useEffect(() => {
    const root = document.documentElement
    if (theme === 'system') root.removeAttribute('data-theme')
    else root.setAttribute('data-theme', theme)
  }, [theme])

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next)
    if (next === 'system') localStorage.removeItem(STORAGE_KEY)
    else localStorage.setItem(STORAGE_KEY, next)
  }, [])

  const value = useMemo<ThemeContextValue>(
    () => ({
      theme,
      setTheme,
      resolved: theme === 'system' ? (systemDark ? 'dark' : 'light') : theme,
    }),
    [setTheme, systemDark, theme],
  )

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}

export function useTheme(): ThemeContextValue {
  const value = useContext(ThemeContext)
  if (!value) throw new Error('useTheme must be used inside ThemeProvider')
  return value
}
